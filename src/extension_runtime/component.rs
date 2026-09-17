use std::collections::BTreeSet;
use std::path::Path;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::{Duration, Instant};

use super::{
    ExtensionRuntime, NegotiatedRuntimeDescriptor, RuntimeError, RuntimeGrants, RuntimeHostCallV1,
    RuntimeIdentityV1, RuntimeRequestV1, RuntimeStopReason, SharedRuntimeHostCalls,
    HANDSHAKE_TIMEOUT, MAX_RUNTIME_FRAME_BYTES,
};
use wasmtime::component::{Component, HasSelf, Linker};
use wasmtime::{Config, Engine, Store, StoreLimits, StoreLimitsBuilder, Trap};

wasmtime::component::bindgen!({
    path: "wit/vqro-extension-service",
    world: "service",
    imports: { default: trappable },
});

const COMPONENT_MEMORY_LIMIT: usize = 64 * 1024 * 1024;
const COMPONENT_TABLE_LIMIT: usize = 4_096;
// A single component instance may contain a bounded graph of lowered core instances.
const COMPONENT_INSTANCE_LIMIT: usize = 16;
const COMPONENT_TABLE_COUNT_LIMIT: usize = 4;
const COMPONENT_MEMORY_COUNT_LIMIT: usize = 1;
const COMPONENT_STACK_LIMIT: usize = 1024 * 1024;
const COMPONENT_STARTUP_FUEL: u64 = 1_000_000_000;
const COMPONENT_FUEL_PER_INVOCATION: u64 = 1_000_000_000;
const COMPONENT_LOG_BYTES_PER_INVOCATION: usize = 64 * 1024;
const INTERRUPT_POLL_INTERVAL: Duration = Duration::from_millis(1);

#[derive(Clone, Debug)]
pub(crate) struct ComponentRuntimeFactory {
    engine: Engine,
    component: Component,
    active: Arc<AtomicBool>,
    package_id: String,
    namespace: String,
    service_id: String,
    grants: RuntimeGrants,
    fuel_per_invocation: u64,
}

impl ComponentRuntimeFactory {
    pub(crate) fn installed(
        installed: &crate::extension_package::InstalledPackageV1,
        service_id: &str,
        namespace: &str,
    ) -> Result<Self, RuntimeError> {
        crate::extension_package::verify_installed_package(
            Path::new(&installed.immutable_root),
            &installed.package_sha256,
        )
        .map_err(RuntimeError::Configuration)?;
        let archive = Path::new(&installed.immutable_root).join("package.vqrox");
        let package = crate::extension_package::validate_package_file(
            &archive,
            Some(&installed.package_sha256),
        )
        .map_err(RuntimeError::Configuration)?;
        if package.descriptor.package_sha256 != installed.package_sha256
            || package.descriptor.manifest_sha256 != installed.manifest_sha256
            || package.descriptor.manifest.id != installed.package_id
            || package.descriptor.manifest.version != installed.version
        {
            return Err(RuntimeError::Configuration(
                "installed package descriptor does not match its verified artifact".into(),
            ));
        }
        if !package
            .descriptor
            .manifest
            .namespaces
            .iter()
            .any(|owned| owned == namespace)
        {
            return Err(RuntimeError::Configuration(
                "selected runtime namespace is not package-owned".into(),
            ));
        }
        let service = package
            .descriptor
            .manifest
            .services
            .iter()
            .find(|service| service.id == service_id)
            .ok_or_else(|| RuntimeError::Configuration("package service is not declared".into()))?;
        let (payload_path, world) = match &service.runtime {
            crate::extension_package::PackageRuntimeV1::Component { component, world } => {
                (component, world)
            }
            crate::extension_package::PackageRuntimeV1::Process { .. } => {
                return Err(RuntimeError::Unsupported(
                    "package service does not use the component runtime".into(),
                ));
            }
        };
        if world != "vqro:extension/service@1.0.0" {
            return Err(RuntimeError::Unsupported(
                "component service must use exactly vqro:extension/service@1.0.0".into(),
            ));
        }
        let payload = package.file(payload_path).ok_or_else(|| {
            RuntimeError::Configuration(
                "component payload is absent from the verified package archive".into(),
            )
        })?;
        if payload.len() > MAX_RUNTIME_FRAME_BYTES * 32 {
            return Err(RuntimeError::ResourceLimit(
                "component payload exceeds the 8 MiB compile limit".into(),
            ));
        }
        let engine = component_engine()?;
        let component = Component::from_binary(&engine, payload)
            .map_err(|error| RuntimeError::Configuration(format!("compile component: {error}")))?;
        let capabilities = service.capabilities.iter().cloned().collect();
        let mut legacy = service
            .capabilities
            .iter()
            .chain(&service.provides)
            .filter_map(|capability| super::process::legacy_capability(capability))
            .collect::<Vec<_>>();
        legacy.sort_by_key(|capability| format!("{capability:?}"));
        legacy.dedup();
        Ok(Self {
            engine,
            component,
            active: Arc::new(AtomicBool::new(false)),
            package_id: installed.package_id.clone(),
            namespace: namespace.into(),
            service_id: service.id.clone(),
            grants: RuntimeGrants {
                legacy,
                capabilities,
                installed_package: true,
            },
            fuel_per_invocation: COMPONENT_FUEL_PER_INVOCATION,
        })
    }

    pub(crate) fn identity(&self, generation: u64) -> RuntimeIdentityV1 {
        RuntimeIdentityV1 {
            package_id: self.package_id.clone(),
            namespace: self.namespace.clone(),
            service_id: self.service_id.clone(),
            generation,
        }
    }

    pub(crate) fn grants(&self) -> RuntimeGrants {
        self.grants.clone()
    }

    pub(crate) fn start(
        &self,
        generation: u64,
        canceled: &AtomicBool,
    ) -> Result<ComponentRuntime, RuntimeError> {
        ComponentRuntime::instantiate(self, generation, canceled)
    }
}

struct ComponentState {
    limits: StoreLimits,
    host_calls: Option<SharedRuntimeHostCalls>,
    canceled: Arc<AtomicBool>,
    log_bytes: usize,
}

impl vqro::extension::types::Host for ComponentState {}

impl vqro::extension::host::Host for ComponentState {
    fn call(
        &mut self,
        request: Vec<u8>,
    ) -> wasmtime::Result<Result<Vec<u8>, vqro::extension::host::HostError>> {
        if request.len() > MAX_RUNTIME_FRAME_BYTES {
            return Ok(Err(vqro::extension::host::HostError {
                code: "frame_too_large".into(),
                message: "component host-call request exceeds the frame limit".into(),
            }));
        }
        let call: RuntimeHostCallV1 = match decode_canonical_json(&request, "host-call request") {
            Ok(call) => call,
            Err(error) => {
                return Ok(Err(vqro::extension::host::HostError {
                    code: error.code().into(),
                    message: error.to_string(),
                }));
            }
        };
        let response = self
            .host_calls
            .as_ref()
            .ok_or_else(|| wasmtime::Error::msg("host-call router is unavailable"))?
            .handle(call);
        let bytes = canonical_json_bytes(&response)?;
        if bytes.len() > MAX_RUNTIME_FRAME_BYTES {
            return Ok(Err(vqro::extension::host::HostError {
                code: "frame_too_large".into(),
                message: "component host-call response exceeds the frame limit".into(),
            }));
        }
        Ok(Ok(bytes))
    }

    fn log(
        &mut self,
        level: vqro::extension::host::LogLevel,
        message: String,
    ) -> wasmtime::Result<()> {
        if message.len() > MAX_RUNTIME_FRAME_BYTES {
            return Err(wasmtime::Error::msg(
                "component log message exceeds the frame limit",
            ));
        }
        self.log_bytes = self.log_bytes.saturating_add(message.len());
        if self.log_bytes > COMPONENT_LOG_BYTES_PER_INVOCATION {
            return Err(wasmtime::Error::msg("component log quota exceeded"));
        }
        tracing::debug!(?level, message = %message, "extension component log");
        Ok(())
    }

    fn cancelled(&mut self) -> wasmtime::Result<bool> {
        Ok(self.canceled.load(Ordering::Acquire))
    }
}

struct ComponentRuntimeLease(Arc<AtomicBool>);

impl ComponentRuntimeLease {
    fn acquire(active: &Arc<AtomicBool>) -> Result<Self, RuntimeError> {
        active
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| {
                RuntimeError::ResourceLimit("component generation is already active".into())
            })?;
        Ok(Self(Arc::clone(active)))
    }
}

impl Drop for ComponentRuntimeLease {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

pub(crate) struct ComponentRuntime {
    engine: Engine,
    store: Store<ComponentState>,
    bindings: Service,
    descriptor: NegotiatedRuntimeDescriptor,
    retired: bool,
    lease: Option<ComponentRuntimeLease>,
    fuel_per_invocation: u64,
}

impl ComponentRuntime {
    fn instantiate(
        factory: &ComponentRuntimeFactory,
        generation: u64,
        canceled: &AtomicBool,
    ) -> Result<Self, RuntimeError> {
        let lease = ComponentRuntimeLease::acquire(&factory.active)?;
        // Factories admit one generation at a time, keeping epoch interruption
        // generation-local while retaining checksum-bound compiled code.
        let engine = factory.engine.clone();
        let linker = component_linker(&engine)?;
        let pre = linker
            .instantiate_pre(&factory.component)
            .map_err(|error| RuntimeError::Configuration(format!("pre-link component: {error}")))?;
        let pre = ServicePre::new(pre).map_err(|error| {
            RuntimeError::Configuration(format!("bind component exports: {error}"))
        })?;
        let canceled_state = Arc::new(AtomicBool::new(false));
        let mut store = Store::new(
            &engine,
            ComponentState {
                limits: component_limits(),
                host_calls: None,
                canceled: Arc::clone(&canceled_state),
                log_bytes: 0,
            },
        );
        store.limiter(|state| &mut state.limits);
        store.set_epoch_deadline(1);
        store.set_hostcall_fuel(MAX_RUNTIME_FRAME_BYTES);
        store
            .set_fuel(COMPONENT_FUEL_PER_INVOCATION)
            .map_err(|error| {
                RuntimeError::Configuration(format!("initialize component fuel: {error}"))
            })?;
        let startup_deadline = Instant::now() + HANDSHAKE_TIMEOUT;
        let bindings = call_with_interrupt(
            &engine,
            &mut store,
            startup_deadline,
            canceled,
            COMPONENT_STARTUP_FUEL,
            |store| pre.instantiate(store),
        )
        .map_err(normalize_component_startup_result)?;
        let descriptor_result = call_with_interrupt(
            &engine,
            &mut store,
            startup_deadline,
            canceled,
            factory.fuel_per_invocation,
            |store| bindings.call_descriptor(store),
        )?;
        let descriptor = descriptor_result.map_err(|error| {
            RuntimeError::Protocol(format!(
                "component descriptor rejected startup [{}]: {}",
                error.code, error.message
            ))
        })?;
        if descriptor.service_id != factory.service_id {
            return Err(RuntimeError::Protocol(
                "component descriptor service identity mismatch".into(),
            ));
        }
        if descriptor.methods.len() > 128
            || descriptor
                .methods
                .iter()
                .any(|method| method.is_empty() || method.len() > 128)
        {
            return Err(RuntimeError::Protocol(
                "component descriptor methods exceed limits".into(),
            ));
        }
        let methods = descriptor.methods.into_iter().collect::<BTreeSet<_>>();
        Ok(Self {
            engine,
            store,
            bindings,
            descriptor: NegotiatedRuntimeDescriptor {
                identity: factory.identity(generation),
                methods,
            },
            retired: false,
            lease: Some(lease),
            fuel_per_invocation: factory.fuel_per_invocation,
        })
    }
}

impl ExtensionRuntime for ComponentRuntime {
    fn descriptor(&self) -> &NegotiatedRuntimeDescriptor {
        &self.descriptor
    }

    fn invoke(
        &mut self,
        request: &RuntimeRequestV1,
        deadline: Instant,
        canceled: &AtomicBool,
        host_calls: &SharedRuntimeHostCalls,
    ) -> Result<serde_json::Value, RuntimeError> {
        if self.retired {
            return Err(RuntimeError::Exited);
        }
        let bytes = canonical_json_bytes(request).map_err(|error| {
            RuntimeError::Protocol(format!("encode canonical component request: {error}"))
        })?;
        if bytes.len() > MAX_RUNTIME_FRAME_BYTES {
            return Err(RuntimeError::Protocol(
                "component request exceeds the frame limit".into(),
            ));
        }
        self.store.data_mut().host_calls = Some(Arc::clone(host_calls));
        self.store.data_mut().log_bytes = 0;
        let result = call_with_interrupt(
            &self.engine,
            &mut self.store,
            deadline,
            canceled,
            self.fuel_per_invocation,
            |store| self.bindings.call_invoke(store, &bytes),
        );
        self.store.data_mut().host_calls = None;
        let result = result?;
        let response = result.map_err(|error| {
            RuntimeError::Remote(format!(
                "component service error [{}]: {}",
                error.code, error.message
            ))
        })?;
        if response.len() > MAX_RUNTIME_FRAME_BYTES {
            return Err(RuntimeError::Protocol(
                "component response exceeds the frame limit".into(),
            ));
        }
        decode_canonical_json(&response, "component response")
    }

    fn stop(&mut self, _reason: RuntimeStopReason) {
        if self.retired {
            return;
        }
        self.retired = true;
        self.engine.increment_epoch();
        self.store.data_mut().host_calls = None;
        self.store.data().canceled.store(true, Ordering::Release);
        self.lease.take();
    }
}

fn component_engine() -> Result<Engine, RuntimeError> {
    let mut config = Config::new();
    config
        .wasm_component_model(true)
        .consume_fuel(true)
        .epoch_interruption(true)
        .max_wasm_stack(COMPONENT_STACK_LIMIT);
    Engine::new(&config).map_err(|error| {
        RuntimeError::Configuration(format!("configure component engine: {error}"))
    })
}

fn component_linker(engine: &Engine) -> Result<Linker<ComponentState>, RuntimeError> {
    let mut linker = Linker::new(engine);
    Service::add_to_linker::<_, HasSelf<_>>(&mut linker, |state| state)
        .map_err(|error| RuntimeError::Configuration(format!("link component host: {error}")))?;
    Ok(linker)
}

fn component_limits() -> StoreLimits {
    StoreLimitsBuilder::new()
        .trap_on_grow_failure(true)
        .memory_size(COMPONENT_MEMORY_LIMIT)
        .table_elements(COMPONENT_TABLE_LIMIT)
        .instances(COMPONENT_INSTANCE_LIMIT)
        .tables(COMPONENT_TABLE_COUNT_LIMIT)
        .memories(COMPONENT_MEMORY_COUNT_LIMIT)
        .build()
}

fn call_with_interrupt<T>(
    engine: &Engine,
    store: &mut Store<ComponentState>,
    deadline: Instant,
    canceled: &AtomicBool,
    fuel: u64,
    call: impl FnOnce(&mut Store<ComponentState>) -> wasmtime::Result<T>,
) -> Result<T, RuntimeError> {
    if canceled.load(Ordering::Acquire) {
        return Err(RuntimeError::Canceled);
    }
    if deadline <= Instant::now() {
        return Err(RuntimeError::Timeout);
    }
    store.data().canceled.store(false, Ordering::Release);
    store.set_epoch_deadline(1);
    store.set_hostcall_fuel(MAX_RUNTIME_FRAME_BYTES);
    store
        .set_fuel(fuel)
        .map_err(|error| RuntimeError::Protocol(format!("reset component fuel: {error}")))?;
    let done = AtomicBool::new(false);
    let canceled_state = Arc::clone(&store.data().canceled);
    let result = std::thread::scope(|scope| {
        scope.spawn(|| {
            while !done.load(Ordering::Acquire) {
                if canceled.load(Ordering::Acquire) {
                    canceled_state.store(true, Ordering::Release);
                    engine.increment_epoch();
                    return;
                }
                if Instant::now() >= deadline {
                    engine.increment_epoch();
                    return;
                }
                std::thread::sleep(INTERRUPT_POLL_INTERVAL);
            }
        });
        let result = call(store);
        done.store(true, Ordering::Release);
        result
    });
    result.map_err(|error| normalize_component_error(error, deadline, canceled))
}

fn normalize_component_error(
    error: wasmtime::Error,
    deadline: Instant,
    canceled: &AtomicBool,
) -> RuntimeError {
    if canceled.load(Ordering::Acquire) {
        return RuntimeError::Canceled;
    }
    if Instant::now() >= deadline {
        return RuntimeError::Timeout;
    }
    let message = format!("{error:#}");
    if is_resource_limit_message(&message) {
        return RuntimeError::ResourceLimit(message);
    }
    if let Some(trap) = error.downcast_ref::<Trap>() {
        return match trap {
            Trap::OutOfFuel => RuntimeError::FuelExhausted,
            Trap::Interrupt => RuntimeError::Timeout,
            _ => RuntimeError::Trap(trap.to_string()),
        };
    }
    if message.contains("component exceeded the host-call bound") {
        return RuntimeError::Protocol("component exceeded the host-call bound".into());
    }
    if message.contains("fuel allocated for hostcalls has been exhausted") {
        return RuntimeError::Protocol(
            "component request or response exceeds the frame limit".into(),
        );
    }
    if is_resource_limit_message(&message) {
        RuntimeError::ResourceLimit(message)
    } else {
        RuntimeError::Trap(message)
    }
}

fn normalize_component_startup_result(error: RuntimeError) -> RuntimeError {
    match error {
        RuntimeError::Trap(message) => {
            RuntimeError::Trap(format!("instantiate component: {message}"))
        }
        error => error,
    }
}

fn is_resource_limit_message(message: &str) -> bool {
    message.contains("component log quota exceeded")
        || message.contains("resource limit")
        || message.contains("memory minimum size")
        || message.contains("table minimum size")
        || message.contains("failed to grow")
        || message.contains("forcing trap when growing")
        || message.contains("forcing trap when creating")
}

fn decode_canonical_json<T: serde::de::DeserializeOwned>(
    bytes: &[u8],
    label: &str,
) -> Result<T, RuntimeError> {
    let value = serde_json::from_slice::<serde_json::Value>(bytes)
        .map_err(|error| RuntimeError::Protocol(format!("invalid {label} JSON: {error}")))?;
    let canonical = canonical_json_bytes(&value)
        .map_err(|error| RuntimeError::Protocol(format!("encode canonical {label}: {error}")))?;
    if canonical != bytes {
        return Err(RuntimeError::Protocol(format!(
            "{label} is not canonical JSON"
        )));
    }
    serde_json::from_value(value)
        .map_err(|error| RuntimeError::Protocol(format!("invalid {label}: {error}")))
}

fn canonical_json_bytes(value: &impl serde::Serialize) -> Result<Vec<u8>, serde_json::Error> {
    fn sort(value: serde_json::Value) -> serde_json::Value {
        match value {
            serde_json::Value::Object(object) => {
                let mut entries = object.into_iter().collect::<Vec<_>>();
                entries.sort_by(|left, right| left.0.cmp(&right.0));
                serde_json::Value::Object(
                    entries
                        .into_iter()
                        .map(|(key, value)| (key, sort(value)))
                        .collect(),
                )
            }
            serde_json::Value::Array(values) => {
                serde_json::Value::Array(values.into_iter().map(sort).collect())
            }
            value => value,
        }
    }

    serde_json::to_vec(&sort(serde_json::to_value(value)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};
    use std::path::PathBuf;

    const ECHO_COMPONENT: &[u8] =
        include_bytes!("../../tests/fixtures/extension-components/echo.wasm");
    const MALFORMED_COMPONENT: &[u8] =
        include_bytes!("../../tests/fixtures/extension-components/malformed.wasm");
    const OVERSIZED_COMPONENT: &[u8] =
        include_bytes!("../../tests/fixtures/extension-components/oversized.wasm");
    const TRAP_COMPONENT: &[u8] =
        include_bytes!("../../tests/fixtures/extension-components/trap.wasm");
    const LOOP_COMPONENT: &[u8] =
        include_bytes!("../../tests/fixtures/extension-components/loop.wasm");
    const HOST_CALL_COMPONENT: &[u8] =
        include_bytes!("../../tests/fixtures/extension-components/host-call.wasm");
    const LOG_LIMIT_COMPONENT: &[u8] =
        include_bytes!("../../tests/fixtures/extension-components/log-limit.wasm");
    const HOST_CALL_LIMIT_COMPONENT: &[u8] =
        include_bytes!("../../tests/fixtures/extension-components/host-call-limit.wasm");
    const MEMORY_GROW_COMPONENT: &[u8] =
        include_bytes!("../../tests/fixtures/extension-components/memory-grow.wasm");
    const MEMORY_LIMIT_COMPONENT: &[u8] =
        include_bytes!("../../tests/fixtures/extension-components/memory-limit.wasm");
    const STACK_LIMIT_COMPONENT: &[u8] =
        include_bytes!("../../tests/fixtures/extension-components/stack-limit.wasm");
    const STARTUP_LOOP_COMPONENT: &[u8] =
        include_bytes!("../../tests/fixtures/extension-components/startup-loop.wasm");
    const TABLE_LIMIT_COMPONENT: &[u8] =
        include_bytes!("../../tests/fixtures/extension-components/table-limit.wasm");

    const ECHO_BODY: &str = r#"
    i32.const 64
    i32.const 0
    i32.store8
    global.get $calls
    i32.eqz
    if
      i32.const 68
      i32.const 300
      i32.store
      i32.const 72
      i32.const 25
      i32.store
    else
      global.get $calls
      i32.const 1
      i32.eq
      if
        i32.const 68
        i32.const 340
        i32.store
        i32.const 72
        i32.const 2
        i32.store
      else
        i32.const 68
        i32.const 400
        i32.store
        i32.const 72
        i32.const 36
        i32.store
      end
    end
    global.get $calls
    i32.const 1
    i32.add
    global.set $calls
    i32.const 64
"#;

    const MEMORY_GROW_BODY: &str = r#"
    i32.const 2000 memory.grow drop
    i32.const 64 i32.const 0 i32.store8
    i32.const 68 i32.const 340 i32.store
    i32.const 72 i32.const 2 i32.store
    i32.const 64
"#;

    const STACK_LIMIT_DECLARATION: &str = "(func $recurse (call $recurse))\n  (memory";
    const STACK_LIMIT_BODY: &str = "call $recurse unreachable";
    const STARTUP_LOOP_DECLARATION: &str =
        "(func $startup (loop $forever br $forever)) (start $startup)\n  (memory";

    const MALFORMED_BODY: &str = r#"
    i32.const 64 i32.const 0 i32.store8
    i32.const 68 i32.const 360 i32.store
    i32.const 72 i32.const 8 i32.store
    i32.const 64
"#;

    const OVERSIZED_BODY: &str = r#"
    i32.const 64 i32.const 0 i32.store8
    i32.const 68 i32.const 1024 i32.store
    i32.const 72 i32.const 262145 i32.store
    i32.const 64
"#;

    const TRAP_BODY: &str = "unreachable";
    const LOOP_BODY: &str = "loop $spin br $spin end i32.const 64";

    const LOG_LIMIT_BODY: &str = r#"
    i32.const 2 i32.const 1024 i32.const 65537 call $host-log
    i32.const 64
"#;

    const HOST_CALL_LIMIT_BODY: &str = r#"
    i32.const 500 i32.const 254 i32.const 800 call $host-call
    i32.const 500 i32.const 254 i32.const 800 call $host-call
    i32.const 500 i32.const 254 i32.const 800 call $host-call
    i32.const 500 i32.const 254 i32.const 800 call $host-call
    i32.const 500 i32.const 254 i32.const 800 call $host-call
    i32.const 500 i32.const 254 i32.const 800 call $host-call
    i32.const 500 i32.const 254 i32.const 800 call $host-call
    i32.const 500 i32.const 254 i32.const 800 call $host-call
    i32.const 500 i32.const 254 i32.const 800 call $host-call
    i32.const 64
"#;

    const HOST_CALL_BODY: &str = r#"
    i32.const 500 i32.const 254 i32.const 800 call $host-call
    i32.const 800 i32.load8_u
    if unreachable end
    i32.const 64 i32.const 0 i32.store8
    i32.const 68 i32.const 804 i32.load i32.store
    i32.const 72 i32.const 808 i32.load i32.store
    i32.const 64
"#;

    fn sha256(bytes: &[u8]) -> String {
        format!("{:x}", Sha256::digest(bytes))
    }

    fn component_bytes_from_wat(wat: &str) -> Vec<u8> {
        let mut module = wat::parse_str(wat).unwrap();
        let mut resolve = wit_parser::Resolve::default();
        let (package, _) = resolve.push_dir("wit/vqro-extension-service").unwrap();
        let world = resolve.select_world(&[package], Some("service")).unwrap();
        wit_component::embed_component_metadata(
            &mut module,
            &resolve,
            world,
            wit_component::StringEncoding::UTF8,
        )
        .unwrap();
        wit_component::ComponentEncoder::default()
            .module(&module)
            .unwrap()
            .validate(true)
            .encode()
            .unwrap()
    }

    fn component_bytes(body: &str) -> Vec<u8> {
        let wat = include_str!("../../tests/fixtures/extension-component-service.wat")
            .replace(";; INVOKE_BODY", body);
        component_bytes_from_wat(&wat)
    }

    fn component_with_replacement(body: &str, old: &str, new: &str) -> Vec<u8> {
        let wat = include_str!("../../tests/fixtures/extension-component-service.wat")
            .replace(";; INVOKE_BODY", body)
            .replace(old, new);
        component_bytes_from_wat(&wat)
    }

    fn package_bytes_with_world(component: &[u8], capabilities: &[&str], world: &str) -> Vec<u8> {
        package_bytes_with_namespaces(component, capabilities, world, &["example.component"])
    }

    fn package_bytes_with_namespaces(
        component: &[u8],
        capabilities: &[&str],
        world: &str,
        namespaces: &[&str],
    ) -> Vec<u8> {
        let capabilities = capabilities
            .iter()
            .map(|value| serde_json::to_string(value).unwrap())
            .collect::<Vec<_>>()
            .join(", ");
        let namespaces = namespaces
            .iter()
            .map(|value| serde_json::to_string(value).unwrap())
            .collect::<Vec<_>>()
            .join(", ");
        let manifest = format!(
            "manifest_version = 2\npackage_contract = \"vqro.package.v1\"\nid = \"example.component\"\nversion = \"1.0.0\"\nmin_vqro_version = \"0.1.0\"\nnamespaces = [{namespaces}]\n\n[[services]]\nid = \"echo\"\nprovides = []\ncapabilities = [{capabilities}]\n\n[services.runtime]\nkind = \"component\"\ncomponent = \"services/echo.wasm\"\nworld = \"{world}\"\n"
        );
        let mut files = vec![
            (
                crate::extension_package::PACKAGE_MANIFEST_FILE.to_string(),
                manifest.into_bytes(),
                0o644,
            ),
            ("services/echo.wasm".into(), component.to_vec(), 0o644),
        ];
        files.sort_by(|left, right| left.0.cmp(&right.0));
        let checksums = files
            .iter()
            .map(|(path, bytes, _)| format!("{}  {path}\n", sha256(bytes)))
            .collect::<String>();
        files.push((
            crate::extension_package::PACKAGE_CHECKSUM_FILE.into(),
            checksums.into_bytes(),
            0o644,
        ));
        files.sort_by(|left, right| left.0.cmp(&right.0));
        let mut archive = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut archive);
            builder.mode(tar::HeaderMode::Deterministic);
            for (path, bytes, mode) in files {
                let mut header = tar::Header::new_ustar();
                header.set_path(path).unwrap();
                header.set_size(bytes.len() as u64);
                header.set_mode(mode);
                header.set_uid(0);
                header.set_gid(0);
                header.set_mtime(0);
                header.set_cksum();
                builder.append(&header, bytes.as_slice()).unwrap();
            }
            builder.finish().unwrap();
        }
        archive
    }

    fn package_bytes(component: &[u8], capabilities: &[&str]) -> Vec<u8> {
        package_bytes_with_world(component, capabilities, "vqro:extension/service@1.0.0")
    }

    fn install_archive(bytes: Vec<u8>) -> (PathBuf, crate::extension_package::InstalledPackageV1) {
        let mut random = [0_u8; 8];
        crate::platform::fill_secure_random(&mut random).unwrap();
        let suffix = random
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let root = std::env::temp_dir().join(format!(
            "vqro-component-test-{}-{suffix}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let digest = sha256(&bytes);
        let source = root.join("component.vqrox");
        std::fs::write(&source, bytes).unwrap();
        let installed = crate::extension_package::install_local_file(
            &source,
            &root.join("store"),
            Some(&digest),
            crate::extension_package::PackageProvenanceV1::File {
                package_sha256: digest.clone(),
            },
            &std::collections::BTreeMap::new(),
            None,
        )
        .unwrap();
        (root, installed)
    }

    fn install_component(
        component: &[u8],
        capabilities: &[&str],
    ) -> (PathBuf, crate::extension_package::InstalledPackageV1) {
        install_archive(package_bytes(component, capabilities))
    }

    #[cfg(unix)]
    fn overwrite(path: &Path, bytes: &[u8]) {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = std::fs::metadata(path).unwrap().permissions();
        permissions.set_mode(0o600);
        std::fs::set_permissions(path, permissions).unwrap();
        std::fs::write(path, bytes).unwrap();
    }

    #[cfg(windows)]
    fn overwrite(path: &Path, bytes: &[u8]) {
        let mut permissions = std::fs::metadata(path).unwrap().permissions();
        permissions.set_readonly(false);
        std::fs::set_permissions(path, permissions).unwrap();
        std::fs::write(path, bytes).unwrap();
    }

    #[cfg(unix)]
    fn cleanup(root: PathBuf) {
        use std::os::unix::fs::PermissionsExt;
        let mut pending = vec![root.clone()];
        while let Some(directory) = pending.pop() {
            let mut permissions = std::fs::metadata(&directory).unwrap().permissions();
            permissions.set_mode(0o700);
            std::fs::set_permissions(&directory, permissions).unwrap();
            for entry in std::fs::read_dir(&directory).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    pending.push(path);
                } else {
                    let mut permissions = std::fs::metadata(&path).unwrap().permissions();
                    permissions.set_mode(0o600);
                    std::fs::set_permissions(path, permissions).unwrap();
                }
            }
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(windows)]
    fn cleanup(root: PathBuf) {
        let mut pending = vec![root.clone()];
        while let Some(directory) = pending.pop() {
            let mut permissions = std::fs::metadata(&directory).unwrap().permissions();
            permissions.set_readonly(false);
            std::fs::set_permissions(&directory, permissions).unwrap();
            for entry in std::fs::read_dir(&directory).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    pending.push(path);
                } else {
                    let mut permissions = std::fs::metadata(&path).unwrap().permissions();
                    permissions.set_readonly(false);
                    std::fs::set_permissions(path, permissions).unwrap();
                }
            }
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    #[ignore = "maintainer-only deterministic fixture regeneration"]
    fn regenerate_checked_in_component_fixtures() {
        let directory = Path::new("tests/fixtures/extension-components");
        std::fs::create_dir_all(directory).unwrap();
        let fixtures = vec![
            ("echo.wasm", component_bytes(ECHO_BODY)),
            ("malformed.wasm", component_bytes(MALFORMED_BODY)),
            ("memory-grow.wasm", component_bytes(MEMORY_GROW_BODY)),
            ("oversized.wasm", component_bytes(OVERSIZED_BODY)),
            ("trap.wasm", component_bytes(TRAP_BODY)),
            ("loop.wasm", component_bytes(LOOP_BODY)),
            ("host-call.wasm", component_bytes(HOST_CALL_BODY)),
            ("log-limit.wasm", component_bytes(LOG_LIMIT_BODY)),
            (
                "host-call-limit.wasm",
                component_bytes(HOST_CALL_LIMIT_BODY),
            ),
            (
                "memory-limit.wasm",
                component_with_replacement(
                    ECHO_BODY,
                    "(memory (export \"cm32p2_memory\") 8 1024)",
                    "(memory (export \"cm32p2_memory\") 1025 1025)",
                ),
            ),
            (
                "stack-limit.wasm",
                component_with_replacement(STACK_LIMIT_BODY, "(memory", STACK_LIMIT_DECLARATION),
            ),
            (
                "startup-loop.wasm",
                component_with_replacement(ECHO_BODY, "(memory", STARTUP_LOOP_DECLARATION),
            ),
            (
                "table-limit.wasm",
                component_with_replacement(
                    ECHO_BODY,
                    "(memory (export \"cm32p2_memory\") 8 1024)",
                    "(memory (export \"cm32p2_memory\") 8 1024)\n  (table 4097 funcref)",
                ),
            ),
        ];
        let mut checksums = String::new();
        for (name, bytes) in fixtures {
            std::fs::write(directory.join(name), &bytes).unwrap();
            checksums.push_str(&format!("{}  {name}\n", sha256(&bytes)));
        }
        std::fs::write(directory.join("SHA256SUMS"), checksums).unwrap();
    }

    #[test]
    fn checked_in_component_fixtures_match_recorded_digests() {
        let expected = include_str!("../../tests/fixtures/extension-components/SHA256SUMS");
        let fixtures = [
            ("echo.wasm", ECHO_COMPONENT),
            ("malformed.wasm", MALFORMED_COMPONENT),
            ("memory-grow.wasm", MEMORY_GROW_COMPONENT),
            ("oversized.wasm", OVERSIZED_COMPONENT),
            ("trap.wasm", TRAP_COMPONENT),
            ("loop.wasm", LOOP_COMPONENT),
            ("host-call.wasm", HOST_CALL_COMPONENT),
            ("log-limit.wasm", LOG_LIMIT_COMPONENT),
            ("host-call-limit.wasm", HOST_CALL_LIMIT_COMPONENT),
            ("memory-limit.wasm", MEMORY_LIMIT_COMPONENT),
            ("stack-limit.wasm", STACK_LIMIT_COMPONENT),
            ("startup-loop.wasm", STARTUP_LOOP_COMPONENT),
            ("table-limit.wasm", TABLE_LIMIT_COMPONENT),
        ];
        let actual = fixtures
            .into_iter()
            .map(|(name, bytes)| format!("{}  {name}\n", sha256(bytes)))
            .collect::<String>();
        assert_eq!(actual, expected);
    }

    #[test]
    fn explicit_component_namespace_is_invariant_under_manifest_order() {
        let component = component_bytes(ECHO_BODY);
        let mut identities = Vec::new();
        for namespaces in [
            ["example.alpha", "example.beta"],
            ["example.beta", "example.alpha"],
        ] {
            let (root, installed) = install_archive(package_bytes_with_namespaces(
                &component,
                &[],
                "vqro:extension/service@1.0.0",
                &namespaces,
            ));
            let factory =
                super::super::RuntimeFactory::installed(&installed, "echo", "example.beta")
                    .unwrap();
            identities.push(factory.identity(7));
            assert!(
                super::super::RuntimeFactory::installed(&installed, "echo", "example.missing",)
                    .is_err()
            );
            cleanup(root);
        }
        assert_eq!(identities[0].namespace, "example.beta");
        assert_eq!(identities[0], identities[1]);
    }

    #[test]
    fn component_preflight_rejects_wrong_world_and_non_component_payload() {
        let wrong_world =
            package_bytes_with_world(ECHO_COMPONENT, &[], "vqro:extension/service@1.1.0");
        assert!(
            crate::extension_package::validate_package_bytes(&wrong_world, None)
                .unwrap_err()
                .contains("invalid payload or world")
        );

        let core_module = wat::parse_str("(module)").unwrap();
        let (core_root, installed) = install_component(&core_module, &[]);
        assert!(matches!(
            super::super::RuntimeFactory::installed(&installed, "echo", "example.component"),
            Err(RuntimeError::Configuration(message)) if message.contains("compile component")
        ));
        cleanup(core_root);
    }

    #[test]
    fn component_preflight_enforces_capability_method_negotiation() {
        let (root, installed) = install_component(ECHO_COMPONENT, &["host_document"]);
        let factory =
            super::super::RuntimeFactory::installed(&installed, "echo", "example.component")
                .unwrap();
        let canceled = AtomicBool::new(false);
        assert!(matches!(
            super::super::ExtensionRuntimeDispatcher::start(&factory, 32, &canceled),
            Err(RuntimeError::Protocol(message)) if message.contains("required by its grants")
        ));
        cleanup(root);
    }

    #[test]
    fn component_preflight_enforces_memory_and_table_limits() {
        let canceled = AtomicBool::new(false);
        for component in [MEMORY_LIMIT_COMPONENT, TABLE_LIMIT_COMPONENT] {
            let (root, installed) = install_component(component, &[]);
            let factory =
                super::super::RuntimeFactory::installed(&installed, "echo", "example.component")
                    .unwrap();
            let error = super::super::ExtensionRuntimeDispatcher::start(&factory, 30, &canceled)
                .unwrap_err();
            assert!(
                matches!(&error, RuntimeError::ResourceLimit(_)),
                "{error:?}"
            );
            cleanup(root);
        }
    }

    #[test]
    fn component_passes_shared_conformance_vectors() {
        let (root, installed) = install_component(ECHO_COMPONENT, &[]);
        crate::extension_service::ExtensionServiceSupervisor::preflight_installed_service(
            &installed,
            "echo",
            "example.component",
        )
        .unwrap();
        let factory =
            super::super::RuntimeFactory::installed(&installed, "echo", "example.component")
                .unwrap();
        assert!(matches!(
            factory,
            super::super::RuntimeFactory::Component(_)
        ));
        let startup_canceled = AtomicBool::new(true);
        assert!(matches!(
            super::super::ExtensionRuntimeDispatcher::start(&factory, 20, &startup_canceled),
            Err(RuntimeError::Canceled)
        ));
        let canceled = AtomicBool::new(false);
        let mut dispatcher =
            super::super::ExtensionRuntimeDispatcher::start(&factory, 21, &canceled).unwrap();
        assert!(matches!(
            dispatcher.invoke(
                "conformance.echo",
                serde_json::json!({"oversized": "x".repeat(MAX_RUNTIME_FRAME_BYTES)}),
                21,
                Instant::now() + Duration::from_millis(750),
                &canceled,
            ),
            Err(RuntimeError::Protocol(message)) if message.contains("frame limit")
        ));
        for vector in super::super::shared_conformance_vectors() {
            let response = dispatcher
                .invoke(
                    vector.method,
                    vector.params,
                    21,
                    Instant::now() + Duration::from_millis(750),
                    &canceled,
                )
                .unwrap();
            assert_eq!(response, vector.expected, "{}", vector.name);
        }
        dispatcher.stop(RuntimeStopReason::Requested);
        cleanup(root);
    }

    #[test]
    fn component_restart_executes_only_factory_bound_verified_bytes() {
        let (root, installed) = install_component(ECHO_COMPONENT, &[]);
        let factory =
            super::super::RuntimeFactory::installed(&installed, "echo", "example.component")
                .unwrap();
        overwrite(
            &Path::new(&installed.immutable_root).join("package.vqrox"),
            b"replacement bytes must never be compiled or executed",
        );
        let canceled = AtomicBool::new(false);
        let mut dispatcher =
            super::super::ExtensionRuntimeDispatcher::start(&factory, 29, &canceled).unwrap();
        assert_eq!(
            dispatcher
                .invoke(
                    "conformance.echo",
                    serde_json::json!({"text": "vqro", "count": 2}),
                    29,
                    Instant::now() + Duration::from_millis(750),
                    &canceled,
                )
                .unwrap(),
            serde_json::json!({"text": "vqro", "count": 2})
        );
        dispatcher.stop(RuntimeStopReason::Requested);
        drop(dispatcher);
        let mut replacement =
            super::super::ExtensionRuntimeDispatcher::start(&factory, 30, &canceled).unwrap();
        assert_eq!(
            replacement
                .invoke(
                    "conformance.echo",
                    serde_json::json!({"text": "bound"}),
                    30,
                    Instant::now() + Duration::from_millis(750),
                    &canceled,
                )
                .unwrap(),
            serde_json::json!({"text": "vqro", "count": 2})
        );
        cleanup(root);
    }

    #[test]
    fn component_normalizes_malformed_oversized_and_trapping_results() {
        let canceled = AtomicBool::new(false);
        for (component, expected_code) in [
            (MALFORMED_COMPONENT, "protocol"),
            (MEMORY_GROW_COMPONENT, "resource_limit"),
            (OVERSIZED_COMPONENT, "protocol"),
            (LOG_LIMIT_COMPONENT, "resource_limit"),
            (STACK_LIMIT_COMPONENT, "trap"),
            (TRAP_COMPONENT, "trap"),
        ] {
            let (root, installed) = install_component(component, &[]);
            let factory =
                super::super::RuntimeFactory::installed(&installed, "echo", "example.component")
                    .unwrap();
            let mut dispatcher =
                super::super::ExtensionRuntimeDispatcher::start(&factory, 22, &canceled).unwrap();
            let error = dispatcher
                .invoke(
                    "conformance.echo",
                    serde_json::json!({}),
                    22,
                    Instant::now() + Duration::from_millis(750),
                    &canceled,
                )
                .unwrap_err();
            assert_eq!(error.code(), expected_code, "{error:?}");
            assert!(matches!(
                dispatcher.invoke(
                    "conformance.echo",
                    serde_json::json!({}),
                    22,
                    Instant::now() + Duration::from_millis(750),
                    &canceled,
                ),
                Err(RuntimeError::Exited)
            ));
            cleanup(root);
        }
    }

    #[test]
    fn component_epoch_interrupts_deadline_and_cancellation() {
        let (deadline_root, installed) = install_component(LOOP_COMPONENT, &[]);
        let factory =
            super::super::RuntimeFactory::installed(&installed, "echo", "example.component")
                .unwrap();
        let canceled = AtomicBool::new(false);
        let mut dispatcher =
            super::super::ExtensionRuntimeDispatcher::start(&factory, 23, &canceled).unwrap();
        assert!(matches!(
            dispatcher.invoke(
                "conformance.echo",
                serde_json::json!({}),
                23,
                Instant::now() + Duration::from_millis(40),
                &canceled,
            ),
            Err(RuntimeError::Timeout)
        ));
        assert!(matches!(
            dispatcher.invoke(
                "conformance.echo",
                serde_json::json!({}),
                23,
                Instant::now() + Duration::from_millis(40),
                &canceled,
            ),
            Err(RuntimeError::Exited)
        ));
        cleanup(deadline_root);

        let (cancel_root, installed) = install_component(LOOP_COMPONENT, &[]);
        let factory =
            super::super::RuntimeFactory::installed(&installed, "echo", "example.component")
                .unwrap();
        let canceled = AtomicBool::new(false);
        let mut dispatcher =
            super::super::ExtensionRuntimeDispatcher::start(&factory, 24, &canceled).unwrap();
        std::thread::scope(|scope| {
            scope.spawn(|| {
                std::thread::sleep(Duration::from_millis(20));
                canceled.store(true, Ordering::Release);
            });
            assert!(matches!(
                dispatcher.invoke(
                    "conformance.echo",
                    serde_json::json!({}),
                    24,
                    Instant::now() + Duration::from_millis(750),
                    &canceled,
                ),
                Err(RuntimeError::Canceled)
            ));
        });
        canceled.store(false, Ordering::Release);
        assert!(matches!(
            dispatcher.invoke(
                "conformance.echo",
                serde_json::json!({}),
                24,
                Instant::now() + Duration::from_millis(750),
                &canceled,
            ),
            Err(RuntimeError::Exited)
        ));
        cleanup(cancel_root);

        let (startup_root, installed) = install_component(STARTUP_LOOP_COMPONENT, &[]);
        let factory =
            super::super::RuntimeFactory::installed(&installed, "echo", "example.component")
                .unwrap();
        let canceled = AtomicBool::new(false);
        std::thread::scope(|scope| {
            scope.spawn(|| {
                std::thread::sleep(Duration::from_millis(20));
                canceled.store(true, Ordering::Release);
            });
            assert!(matches!(
                super::super::ExtensionRuntimeDispatcher::start(&factory, 25, &canceled),
                Err(RuntimeError::Canceled)
            ));
        });
        cleanup(startup_root);
    }

    #[test]
    fn component_fuel_exhaustion_is_typed_and_retires_instance() {
        let (root, installed) = install_component(LOOP_COMPONENT, &[]);
        let factory =
            ComponentRuntimeFactory::installed(&installed, "echo", "example.component").unwrap();
        let canceled = AtomicBool::new(false);
        let mut runtime = factory.start(31, &canceled).unwrap();
        runtime.fuel_per_invocation = 1;
        let identity = factory.identity(31);
        let host_calls = Arc::new(super::super::RuntimeHostCallRouter::new(
            identity.clone(),
            identity.namespace.clone(),
            std::collections::BTreeMap::new(),
            super::super::DisabledHostCalls {
                identity: identity.clone(),
                grants: factory.grants(),
            },
        ));
        let request = RuntimeRequestV1 {
            contract: super::super::EXTENSION_SERVICE_CONTRACT_V1.into(),
            request_id: "1".into(),
            identity,
            method: "conformance.echo".into(),
            params: serde_json::json!({}),
        };
        assert!(matches!(
            runtime.invoke(
                &request,
                Instant::now() + Duration::from_millis(750),
                &canceled,
                &host_calls,
            ),
            Err(RuntimeError::FuelExhausted)
        ));
        runtime.stop(RuntimeStopReason::Protocol);
        assert!(matches!(
            runtime.invoke(
                &request,
                Instant::now() + Duration::from_millis(750),
                &canceled,
                &host_calls,
            ),
            Err(RuntimeError::Exited)
        ));
        cleanup(root);
    }

    #[test]
    fn component_failures_use_shared_supervisor_backoff_and_restart_budget() {
        let (root, installed) = install_component(TRAP_COMPONENT, &[]);
        let mut supervisor =
            crate::extension_service::ExtensionServiceSupervisor::from_installed_service(
                &installed,
                "echo",
                "example.component",
                0,
            )
            .unwrap();
        let mut previous_generation = 0;
        for failure in 1..=4 {
            assert!(matches!(
                supervisor.render_view(serde_json::json!({})),
                Err(RuntimeError::Trap(_))
            ));
            let (status, failures, generation) = supervisor.lifecycle_for_test();
            assert_eq!(failures, failure);
            assert!(generation > previous_generation);
            previous_generation = generation;
            if failure <= 3 {
                assert!(matches!(
                    status,
                    crate::extension_service::ExtensionServiceStatus::Backoff { .. }
                ));
                supervisor.expire_backoff_for_test();
            } else {
                assert!(matches!(
                    status,
                    crate::extension_service::ExtensionServiceStatus::Failed(_)
                ));
            }
        }
        cleanup(root);
    }

    #[test]
    fn component_event_harness_handles_unrelated_state_snapshot_first() {
        let wat = include_str!("../../tests/fixtures/extension-component-service.wat")
            .replace(";; INVOKE_BODY", HOST_CALL_BODY)
            .replace(
                r#"\22capability\22:\22state\22"#,
                r#"\22capability\22:\22host.state.read\22"#,
            )
            .replace(
                r#"\22params\22:{}"#,
                r#"\22params\22:{\22contract\22:\22host.state.v1\22}"#,
            )
            .replace("i32.const 500 i32.const 254", "i32.const 500 i32.const 290");
        let component = component_bytes_from_wat(&wat);
        let (root, installed) = install_component(
            &component,
            &[crate::extension_host_state::HOST_STATE_READ_CAPABILITY],
        );
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = crate::app::App::new(
            &crate::config::Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        let (authorization, _) = crate::extension_host_state::install_test_package_and_authorize(
            &mut app,
            &installed,
            "echo",
            "example.component",
            26,
        );
        let decoy_lease = authorization.bind(
            "wrong-request".into(),
            super::super::RuntimeIdentityV1 {
                package_id: "example.component".into(),
                namespace: "example.component".into(),
                service_id: "echo".into(),
                generation: 26,
            },
            Instant::now() + Duration::from_secs(2),
            Arc::new(AtomicBool::new(false)),
        );
        let (decoy_respond_to, decoy_response) = std::sync::mpsc::sync_channel(1);
        app.event_tx
            .try_send(crate::events::AppEvent::ExtensionHostStateCall(Box::new(
                crate::extension_host_state::HostStateActorRequest {
                    lease: decoy_lease,
                    call_id: "decoy".into(),
                    capability: crate::extension_host_state::HOST_STATE_READ_CAPABILITY.into(),
                    method: crate::extension_host_state::HostStateActorMethod::Snapshot,
                    respond_to: decoy_respond_to,
                },
            )))
            .unwrap();
        let factory =
            super::super::RuntimeFactory::installed(&installed, "echo", "example.component")
                .unwrap();
        let canceled = Arc::new(AtomicBool::new(false));
        let join = std::thread::spawn(move || {
            let mut dispatcher =
                super::super::ExtensionRuntimeDispatcher::start(&factory, 26, canceled.as_ref())
                    .unwrap();
            dispatcher.invoke_with_host_state(
                "conformance.echo",
                serde_json::json!({}),
                26,
                Instant::now() + Duration::from_millis(750),
                canceled,
                authorization,
            )
        });
        let join = super::super::handle_app_events_until(
            &mut app,
            "component host-state snapshot",
            join,
            |event| {
                super::super::host_state_actor_request_matches(
                    event,
                    "1",
                    &super::super::RuntimeIdentityV1 {
                        package_id: "example.component".into(),
                        namespace: "example.component".into(),
                        service_id: "echo".into(),
                        generation: 26,
                    },
                    "call-1",
                    crate::extension_host_state::HOST_STATE_READ_CAPABILITY,
                    crate::extension_host_state::HOST_STATE_SNAPSHOT_METHOD,
                )
            },
        );
        assert!(decoy_response.try_recv().unwrap().is_ok());
        let response = join.join().unwrap().unwrap();
        assert_eq!(response["result"]["contract"], "host.state.v1");
        assert_eq!(response["result"]["namespace"], "example.component");
        cleanup(root);
    }

    #[test]
    fn component_uses_one_composite_state_and_terminal_read_context() {
        let terminal_call = r#"{\22call_id\22:\22terminals\22,\22capability\22:\22host.terminals.read\22,\22depth\22:1,\22identity\22:{\22generation\22:26,\22namespace\22:\22example.component\22,\22package_id\22:\22example.component\22,\22service_id\22:\22echo\22},\22method\22:\22terminals.snapshot\22,\22namespace\22:\22example.component\22,\22params\22:{\22contract\22:\22host.terminals.v1\22},\22type\22:\22host_call\22}"#;
        let state_call = r#"{\22call_id\22:\22state\22,\22capability\22:\22host.state.read\22,\22depth\22:1,\22identity\22:{\22generation\22:26,\22namespace\22:\22example.component\22,\22package_id\22:\22example.component\22,\22service_id\22:\22echo\22},\22method\22:\22state.snapshot\22,\22namespace\22:\22example.component\22,\22params\22:{\22contract\22:\22host.state.v1\22},\22type\22:\22host_call\22}"#;
        let body = r#"
    i32.const 500 i32.const 305 i32.const 2000 call $host-call
    i32.const 2000 i32.load8_u
    if unreachable end
    i32.const 1000 i32.const 289 i32.const 4000 call $host-call
    i32.const 4000 i32.load8_u
    if unreachable end
    i32.const 64 i32.const 0 i32.store8
    i32.const 68 i32.const 4004 i32.load i32.store
    i32.const 72 i32.const 4008 i32.load i32.store
    i32.const 64
"#;
        let mut wat = include_str!("../../tests/fixtures/extension-component-service.wat")
            .replace(";; INVOKE_BODY", body);
        let data_start = wat.find("  (data (i32.const 500)").unwrap();
        let data_end = wat[data_start..].find('\n').unwrap() + data_start;
        wat.replace_range(
            data_start..data_end,
            &format!(
                "  (data (i32.const 500) \"{terminal_call}\")\n  (data (i32.const 1000) \"{state_call}\")"
            ),
        );
        let component = component_bytes_from_wat(&wat);
        let (root, installed) = install_component(
            &component,
            &[
                crate::extension_host_state::HOST_STATE_READ_CAPABILITY,
                crate::extension_host_terminals::HOST_TERMINALS_READ_CAPABILITY,
            ],
        );
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = crate::app::App::new(
            &crate::config::Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        let _ = crate::extension_host_terminals::install_test_package_and_authorize(
            &mut app,
            &installed,
            "echo",
            "example.component",
            26,
        );
        let binding = crate::extension_host_state::HostProviderBinding::from_installed(
            &installed,
            "echo",
            "example.component",
        )
        .unwrap();
        let workspace_id = app.state.workspaces[0].id.clone();
        let tab_id = app.state.workspaces[0].tabs[0].id.clone();
        let authorization = app
            .stage_host_read_authorization(
                &binding,
                crate::extension_host_invocation::HostReadInvocationPurpose {
                    state_snapshot: true,
                    terminals: Some((workspace_id, tab_id)),
                },
                5,
                7,
                26,
            )
            .unwrap();
        let observations = authorization.terminal_observations();
        let factory =
            super::super::RuntimeFactory::installed(&installed, "echo", "example.component")
                .unwrap();
        let canceled = Arc::new(AtomicBool::new(false));
        let join = std::thread::spawn(move || {
            let mut dispatcher =
                super::super::ExtensionRuntimeDispatcher::start(&factory, 26, canceled.as_ref())
                    .unwrap();
            dispatcher.invoke_with_host_reads(
                "conformance.echo",
                serde_json::json!({}),
                26,
                Instant::now() + Duration::from_millis(750),
                canceled,
                authorization,
            )
        });
        let join = super::super::handle_app_events_until(
            &mut app,
            "component composite reads",
            join,
            |event| {
                super::super::host_state_actor_request_matches(
                    event,
                    "1",
                    &super::super::RuntimeIdentityV1 {
                        package_id: "example.component".into(),
                        namespace: "example.component".into(),
                        service_id: "echo".into(),
                        generation: 26,
                    },
                    "state",
                    crate::extension_host_state::HOST_STATE_READ_CAPABILITY,
                    crate::extension_host_state::HOST_STATE_SNAPSHOT_METHOD,
                )
            },
        );
        let response = join.join().unwrap().unwrap();
        assert_eq!(response["result"]["contract"], "host.state.v1");
        let snapshot = observations.exactly_one().unwrap();
        let dependency = crate::host_document_v2::terminal_snapshot_dependency(
            snapshot.tab_id.as_str(),
            &snapshot.fingerprint_sha256,
            snapshot.lease_fence.provider_generation,
        )
        .unwrap();
        assert_eq!(dependency.contract, "host.terminals.v1");
        assert_eq!(dependency.generation, 7);
        cleanup(root);
    }

    #[test]
    fn component_terminal_snapshot_rejects_scope_change_at_final_authorization() {
        let wat = include_str!("../../tests/fixtures/extension-component-service.wat")
            .replace(";; INVOKE_BODY", HOST_CALL_BODY)
            .replace(
                r#"\22capability\22:\22state\22"#,
                r#"\22capability\22:\22host.terminals.read\22"#,
            )
            .replace(
                r#"\22method\22:\22state.snapshot\22"#,
                r#"\22method\22:\22terminals.snapshot\22"#,
            )
            .replace(
                r#"\22params\22:{}"#,
                r#"\22params\22:{\22contract\22:\22host.terminals.v1\22}"#,
            )
            .replace("i32.const 500 i32.const 254", "i32.const 500 i32.const 302");
        let component = component_bytes_from_wat(&wat);
        let (root, installed) = install_component(
            &component,
            &[crate::extension_host_terminals::HOST_TERMINALS_READ_CAPABILITY],
        );
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = crate::app::App::new(
            &crate::config::Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        let authorization = crate::extension_host_terminals::install_test_package_and_authorize(
            &mut app,
            &installed,
            "echo",
            "example.component",
            26,
        );
        let factory =
            super::super::RuntimeFactory::installed(&installed, "echo", "example.component")
                .unwrap();
        let canceled = Arc::new(AtomicBool::new(false));
        let join = std::thread::spawn(move || {
            let mut dispatcher =
                super::super::ExtensionRuntimeDispatcher::start(&factory, 26, canceled.as_ref())
                    .unwrap();
            dispatcher.invoke_with_host_terminals(
                "conformance.echo",
                serde_json::json!({}),
                26,
                Instant::now() + Duration::from_millis(750),
                canceled,
                authorization,
            )
        });
        let expected_identity = super::super::RuntimeIdentityV1 {
            package_id: "example.component".into(),
            namespace: "example.component".into(),
            service_id: "echo".into(),
            generation: 26,
        };
        let capture = super::super::take_host_terminals_actor_request(
            &mut app,
            "1",
            &expected_identity,
            "call-1",
            crate::extension_host_terminals::HOST_TERMINALS_READ_CAPABILITY,
            crate::extension_host_terminals::HostTerminalsActorMethod::Capture,
            "component terminal capture",
        );
        app.handle_extension_host_terminals_call(*capture);
        app.state.workspaces[0].tabs[0].id = crate::workspace::TabId::alloc().unwrap();
        let validate = super::super::take_host_terminals_actor_request(
            &mut app,
            "1",
            &expected_identity,
            "call-1",
            crate::extension_host_terminals::HOST_TERMINALS_READ_CAPABILITY,
            crate::extension_host_terminals::HostTerminalsActorMethod::Validate,
            "component terminal final authorization",
        );
        app.handle_extension_host_terminals_call(*validate);
        let response = join.join().unwrap().unwrap();
        assert_eq!(response["error"]["code"], "stale_host_call_lease");
        cleanup(root);
    }

    #[test]
    fn component_transact_preserves_json_null_and_delete_semantics() {
        let set_call = r#"{\22call_id\22:\22set-null\22,\22capability\22:\22host.state.write\22,\22depth\22:1,\22identity\22:{\22generation\22:26,\22namespace\22:\22example.component\22,\22package_id\22:\22example.component\22,\22service_id\22:\22echo\22},\22method\22:\22state.transact\22,\22namespace\22:\22example.component\22,\22params\22:{\22contract\22:\22host.state.v1\22,\22expected_revision\22:0,\22expected_store_generation\22:1,\22idempotency_key\22:\22set-null\22,\22operations\22:[{\22key\22:\22nullable\22,\22op\22:\22set\22,\22value\22:null}]},\22type\22:\22host_call\22}"#;
        let delete_call = r#"{\22call_id\22:\22delete-null\22,\22capability\22:\22host.state.write\22,\22depth\22:1,\22identity\22:{\22generation\22:26,\22namespace\22:\22example.component\22,\22package_id\22:\22example.component\22,\22service_id\22:\22echo\22},\22method\22:\22state.transact\22,\22namespace\22:\22example.component\22,\22params\22:{\22contract\22:\22host.state.v1\22,\22expected_revision\22:1,\22expected_store_generation\22:1,\22idempotency_key\22:\22delete-null\22,\22operations\22:[{\22key\22:\22nullable\22,\22op\22:\22delete\22}]},\22type\22:\22host_call\22}"#;
        assert_eq!(set_call.replace(r#"\22"#, "\"").len(), 432);
        assert_eq!(delete_call.replace(r#"\22"#, "\"").len(), 428);
        let body = r#"
    global.get $calls
    i32.eqz
    if
      i32.const 500 i32.const 432 i32.const 2000 call $host-call
    else
      i32.const 1200 i32.const 428 i32.const 2000 call $host-call
    end
    i32.const 2000 i32.load8_u
    if unreachable end
    global.get $calls i32.const 1 i32.add global.set $calls
    i32.const 64 i32.const 0 i32.store8
    i32.const 68 i32.const 2004 i32.load i32.store
    i32.const 72 i32.const 2008 i32.load i32.store
    i32.const 64
"#;
        let mut wat = include_str!("../../tests/fixtures/extension-component-service.wat")
            .replace(";; INVOKE_BODY", body);
        let data_start = wat.find("  (data (i32.const 500)").unwrap();
        let data_end = wat[data_start..].find('\n').unwrap() + data_start;
        wat.replace_range(
            data_start..data_end,
            &format!(
                "  (data (i32.const 500) \"{set_call}\")\n  (data (i32.const 1200) \"{delete_call}\")"
            ),
        );
        let component = component_bytes_from_wat(&wat);
        let (root, installed) = install_component(
            &component,
            &[crate::extension_host_state::HOST_STATE_WRITE_CAPABILITY],
        );
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = crate::app::App::new(
            &crate::config::Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        let (authorization, _) = crate::extension_host_state::install_test_package_and_authorize(
            &mut app,
            &installed,
            "echo",
            "example.component",
            26,
        );
        let factory =
            super::super::RuntimeFactory::installed(&installed, "echo", "example.component")
                .unwrap();
        app.event_tx
            .try_send(crate::events::AppEvent::ExtensionCatalogCapacityAvailable)
            .unwrap();
        let canceled = Arc::new(AtomicBool::new(false));
        let mut join = std::thread::spawn(move || {
            let mut dispatcher =
                super::super::ExtensionRuntimeDispatcher::start(&factory, 26, canceled.as_ref())
                    .unwrap();
            let set = dispatcher
                .invoke_with_host_state(
                    "conformance.echo",
                    serde_json::json!({}),
                    26,
                    Instant::now() + Duration::from_millis(750),
                    canceled.clone(),
                    authorization.clone(),
                )
                .unwrap();
            let delete = dispatcher
                .invoke_with_host_state(
                    "conformance.echo",
                    serde_json::json!({}),
                    26,
                    Instant::now() + Duration::from_millis(750),
                    canceled,
                    authorization,
                )
                .unwrap();
            (set, delete)
        });
        for expected_revision in [1, 2] {
            join = super::super::handle_app_events_until(
                &mut app,
                "component host-state transaction",
                join,
                |event| {
                    let outer_request_id = if expected_revision == 1 { "1" } else { "2" };
                    let call_id = if expected_revision == 1 {
                        "set-null"
                    } else {
                        "delete-null"
                    };
                    super::super::host_state_actor_request_matches(
                        event,
                        outer_request_id,
                        &super::super::RuntimeIdentityV1 {
                            package_id: "example.component".into(),
                            namespace: "example.component".into(),
                            service_id: "echo".into(),
                            generation: 26,
                        },
                        call_id,
                        crate::extension_host_state::HOST_STATE_WRITE_CAPABILITY,
                        crate::extension_host_state::HOST_STATE_TRANSACT_METHOD,
                    ) && matches!(
                        event,
                        crate::events::AppEvent::ExtensionHostStateCall(request)
                            if matches!(
                                &request.method,
                                crate::extension_host_state::HostStateActorMethod::Transact(transaction)
                                    if transaction.expected_revision == expected_revision - 1
                                        && transaction.idempotency_key == call_id
                            )
                    )
                },
            );
            assert_eq!(
                app.extension_state
                    .snapshot("example.component")
                    .unwrap()
                    .revision,
                expected_revision
            );
            if expected_revision == 1 {
                assert_eq!(
                    app.extension_state
                        .snapshot("example.component")
                        .unwrap()
                        .values["nullable"],
                    serde_json::Value::Null
                );
            }
        }
        let (set, delete) = join.join().unwrap();
        assert_eq!(set["result"]["revision"], 1);
        assert_eq!(delete["result"]["revision"], 2);
        assert!(!app
            .extension_state
            .snapshot("example.component")
            .unwrap()
            .values
            .contains_key("nullable"));
        cleanup(root);
    }

    #[test]
    fn component_fences_generation_denies_host_call_and_restarts_fresh() {
        let canceled = AtomicBool::new(false);
        let (host_root, installed) = install_component(HOST_CALL_COMPONENT, &[]);
        let factory =
            super::super::RuntimeFactory::installed(&installed, "echo", "example.component")
                .unwrap();
        let mut dispatcher =
            super::super::ExtensionRuntimeDispatcher::start(&factory, 26, &canceled).unwrap();
        assert!(matches!(
            dispatcher.invoke(
                "conformance.echo",
                serde_json::json!({}),
                25,
                Instant::now() + Duration::from_millis(750),
                &canceled,
            ),
            Err(RuntimeError::StaleGeneration {
                expected: 26,
                actual: 25
            })
        ));
        let denied = dispatcher
            .invoke(
                "conformance.echo",
                serde_json::json!({}),
                26,
                Instant::now() + Duration::from_millis(750),
                &canceled,
            )
            .unwrap();
        assert_eq!(denied["error"]["code"], "capability_denied");
        let invocation_canceled = AtomicBool::new(true);
        assert!(matches!(
            dispatcher.invoke(
                "conformance.echo",
                serde_json::json!({}),
                26,
                Instant::now() + Duration::from_millis(750),
                &invocation_canceled,
            ),
            Err(RuntimeError::Canceled)
        ));
        assert!(matches!(
            dispatcher.invoke(
                "conformance.echo",
                serde_json::json!({"oversized": "x".repeat(MAX_RUNTIME_FRAME_BYTES)}),
                26,
                Instant::now() + Duration::from_millis(750),
                &canceled,
            ),
            Err(RuntimeError::Protocol(message)) if message.contains("frame limit")
        ));
        cleanup(host_root);

        let (limit_root, installed) = install_component(HOST_CALL_LIMIT_COMPONENT, &[]);
        let factory =
            super::super::RuntimeFactory::installed(&installed, "echo", "example.component")
                .unwrap();
        let mut limited =
            super::super::ExtensionRuntimeDispatcher::start(&factory, 26, &canceled).unwrap();
        let error = limited
            .invoke(
                "conformance.echo",
                serde_json::json!({}),
                26,
                Instant::now() + Duration::from_millis(750),
                &canceled,
            )
            .unwrap_err();
        assert!(
            matches!(&error, RuntimeError::Protocol(message) if message.contains("host-call bound")),
            "{error:?}"
        );
        assert!(matches!(
            limited.invoke(
                "conformance.echo",
                serde_json::json!({}),
                26,
                Instant::now() + Duration::from_millis(750),
                &canceled,
            ),
            Err(RuntimeError::Exited)
        ));
        cleanup(limit_root);

        let (trap_root, installed) = install_component(TRAP_COMPONENT, &[]);
        let factory =
            super::super::RuntimeFactory::installed(&installed, "echo", "example.component")
                .unwrap();
        let mut old =
            super::super::ExtensionRuntimeDispatcher::start(&factory, 27, &canceled).unwrap();
        assert!(matches!(
            old.invoke(
                "conformance.echo",
                serde_json::json!({}),
                27,
                Instant::now() + Duration::from_millis(750),
                &canceled,
            ),
            Err(RuntimeError::Trap(_))
        ));
        let mut replacement =
            super::super::ExtensionRuntimeDispatcher::start(&factory, 28, &canceled).unwrap();
        assert!(matches!(
            replacement.invoke(
                "conformance.echo",
                serde_json::json!({}),
                28,
                Instant::now() + Duration::from_millis(750),
                &canceled,
            ),
            Err(RuntimeError::Trap(_))
        ));
        cleanup(trap_root);
    }
}
