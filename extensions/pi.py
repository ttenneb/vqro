#!/usr/bin/env python3
"""Pi launch/runtime settings policy. Execution stays in the backend-owned bridge."""
import json
from pathlib import Path
import sys
import time
import uuid

sys.path.insert(0, str(Path(__file__).resolve().parent))
from vqro_ext import Extension, VqroError, command_payload
from pi_testing import testing_notice, testing_environment, rpc_testing_args
from vqro_display import label
from account_credentials import choose as choose_account, environment as account_environment, provider_argv, runtime_environment, require_fresh, snapshot, selection_problem
from accounts_pi import history_file, membership_revision
import accounts_policy as policy
from agent_launch import initialize_request_route, launch, launch_cwd, runtimes as validate_runtimes, valid_argv

SETTINGS = "doc/settings/pi"
LEGACY = "doc/settings/agents"
DEFAULTS = {"runtimes": [{"name": "pi", "argv": ["pi1", "--mode", "rpc"]}]}
EXTENSIONS = str(Path(__file__).resolve().parent)


def default_bridge():
    # Resolved by the terminal host, never selected from the core host's OS.
    # Windows/other hosts configure their launcher explicitly (e.g. py -3).
    return ["python3", "-I", str(Path(EXTENSIONS) / "pi_bridge" / "launch.py")]


def runtimes(value):
    return validate_runtimes(value, brand="Pi")


class Pi:
    def __init__(self, ext):
        self.ext = ext
        for name, fn, title in [("new", self.new, "New Pi agent"), ("resume", self.resume, "Resume acknowledged Pi account cutover"), ("resume.prepare", self.prepare_resume, "Validate Pi resume before stopping"), ("resume.recover", self.recover_resume, "Recover stopped Pi on the intended account"), ("settings", self.settings, "Pi runtime settings"),
                                ("configure", self.configure, "Configure Pi runtime argv")]:
            ext.command("pi."+name, title)(self.guarded(fn))
        def initialize():
            if not ext.version(SETTINGS):
                old = ext.value(LEGACY)
                value = {"runtimes": old["runtimes"]} if isinstance(old, dict) and "runtimes" in old else DEFAULTS
                ext.apply([{"op": "set", "key": SETTINGS, "expect": 0, "value": value}], "pi: migrate/default runtimes")
        ext.retrying(initialize)
        ext.contribute({
            # A compact "+<agent icon>" after the new-tab "+"; the name is in the command list (prefix a, :).
            "menu/pi.new": {"where": "tabbar", "label": "+", "icon_after": True,
                            "icon": {"name": "agent", "nerd": "\U000f06a9", "text": "◆"}, "command": "pi.new", "order": 5},
            "binding/pi.new": {"key": "a", "command": "pi.new", "label": "new Pi agent"},
            "view/accounts-choice-pi-new": {"kind":"panel","title":"Pi account","items":[]},
            "view/pi-runtimes": {"kind": "panel", "title": "Pi runtimes", "items": []},
            "view/pi-settings": {"kind": "panel", "title": "Pi runtime settings", "items": []},
        })

    def guarded(self, fn):
        def command(args):
            try:
                return fn(args)
            except VqroError as error:
                self.ext.notify(label(f"Pi: {error} ({error.kind}); no automatic retry."), "warn" if error.kind in ("timeout", "indeterminate") else "error")
                raise
        return command

    def new(self, args):
        entry=self.ext.vqro.call("workspace.get",{"key":SETTINGS})
        options = runtimes(entry["value"] if entry else DEFAULTS)
        selected = args.get("runtime") or args.get("value")
        cwd = launch_cwd(self.ext, args)
        if selected is None and len(options) > 1:
            self.ext.publish("pi-runtimes", {"title": "New Pi agent: runtime", "requires": ["menu"], "items": [
                {"text": label(r["name"]), "description": label(" ".join(r["argv"])), "command": "pi.new",
                 "args": {"runtime": r["name"], **({"cwd": cwd} if cwd else {}),
                          **{k:args[k] for k in ('account','_account_revision','fresh') if k in args}}} for r in options]})
            return {"client": {"popup": {"view": "pi-runtimes", "width": 72}}}
        runtime = next((r for r in options if r["name"] == selected), None) if selected is not None else options[0]
        if runtime is None:
            raise VqroError("configured runtime not found", "invalid")
        if args.get('fresh') is True: require_fresh('pi', runtime['argv'])
        account, popup = choose_account(self.ext, dict(args, runtime=runtime['name'], **({'cwd':cwd} if cwd else {})), 'pi', 'pi.new')
        if popup: return popup
        native_argv = provider_argv(runtime['argv'], account)
        launcher = runtime.get("bridge_argv", default_bridge())
        # The local isolated launcher locates its installed package itself. Neither it nor
        # a configured host launcher injects a module path into the agent environment.
        env = account_environment(account, runtime_environment('pi', runtime))
        testing = []
        if len(launcher) >= 3 and launcher[-1] == default_bridge()[-1] and "-I" in launcher[1:-1]:
            testing = rpc_testing_args(EXTENSIONS, self.ext)
        else:
            testing_notice(self.ext, "testing plugin unsupported for configured RPC launcher; launching without testing hooks (NOT VERIFIED)")
        selectors = {key: value for key, value in testing_environment(self.ext, EXTENSIONS).items()
                     if key in ("VQRO_TESTING_SOCKET", "VQRO_TESTING_STATE_DIR")}
        for key in ("VQRO_TESTING_SOCKET", "VQRO_TESTING_STATE_DIR"):
            env.pop(key, None)  # A configured/ambient selector is not managed binding evidence.
        if testing and len(selectors) == 2:
            env.update(selectors)
        elif testing:
            testing = []
            testing_notice(self.ext, "testing plugin unavailable: managed socket/state selectors missing; launching without testing hooks (NOT VERIFIED)")
        # A new agent owns its own logical work, not its launching TUI's undo scope.
        env.setdefault("VQRO_PRINCIPAL", "p:"+uuid.uuid4().hex)
        params = {"bridge": True, "name": "pi-bridge-"+uuid.uuid4().hex[:12], "argv": [*launcher, *testing, "--", *native_argv], "env": env}
        if cwd:
            params["cwd"] = cwd
        answer = launch(self.ext, params, brand="Pi", initialize_route=self.initialize_request_route,
                        show_terminal=self.terminal)
        if testing:
            try:
                rows = self.ext.vqro.call("resource.list", {"id": answer.get("agent")})["resources"] if answer.get("agent") else []
                fact = next((row.get("data", {}).get("agent", {}).get("testing_plugin") for row in rows
                             if row.get("id") == answer.get("agent")), None)
            except Exception:
                fact = None
            answer["testing_plugin"] = fact or {"state": "unverified"}
            if not fact or fact.get("state") != "loaded":
                testing_notice(self.ext, "testing plugin loading unconfirmed for this RPC launch (NOT VERIFIED); no launch retry")
        return answer

    def _resume_parameters(self,args,*,require_exit,recovery=False):
        """Shared read-only admission for pre-stop preparation and real resume."""
        args=command_payload(args)
        if not isinstance(args,dict) or set(args)!={'_claim'}:
            raise VqroError('Pi resume requires only its protected claim reference','invalid')
        claim=self.ext.invoke('accounts.claim.read',{'claim':args['_claim']})
        if not isinstance(claim,dict) or claim.get('state')!=('unknown' if recovery else 'claimed') or claim.get('runtime')!='pi':
            raise VqroError('acknowledged Pi restart claim required','forbidden')
        rows=self.ext.vqro.call('resource.list',{'id':claim['resource']}).get('resources',[])
        if len(rows)!=1: raise VqroError('old Pi process evidence unavailable','indeterminate')
        source=rows[0];native=policy.facts(source);session=native.get('session') or {};exited=source.get('exit') or {}
        expected={'host':source.get('host'),'source_incarnation':source.get('incarnation'),
                  'source_epoch':native.get('epoch'),'source_account':policy.configured_id(source),'session':session.get('id'),
                  'source_history':history_file(native),'source_principal':native.get('tool_principal')}
        settled=(native.get('lifecycle')=='ended' and exited.get('success') is True
                 and type(exited.get('code')) is int and exited['code']==0)
        live=(source.get('available') is True and native.get('lifecycle')=='live'
              and native.get('state')=='idle' and native.get('queue')==[])
        if any(claim.get(key)!=value for key,value in expected.items()) or not (settled if require_exit else live):
            raise VqroError('Pi source identity/lifecycle is not ready for this resume phase','indeterminate')
        self.ext.fresh()
        if not isinstance(claim['source_principal'],str) or not claim['source_principal'] or claim['source_principal']==self.ext.owner():
            raise VqroError('original non-owner tool principal is unavailable; never invent a fresh work scope','forbidden')
        accounts,revision=snapshot(self.ext)
        target=next((row for row in accounts if row['id']==claim['account']),None)
        if revision!=claim['_account_revision'] or target is None or selection_problem(target) or target['runtime']!='pi' or target['kind']!='subscription':
            raise VqroError('Pi target credential reference changed','stale')
        entry=self.ext.vqro.call('workspace.get',{'key':SETTINGS})
        options=runtimes(entry['value'] if entry else DEFAULTS)
        if len(options)!=1: raise VqroError('shared Pi runtime is ambiguous','stale')
        model=session.get('model') or {};runtime=options[0]
        if membership_revision(claim['affected'],claim['native_episode'],entry['version'] if entry else 0,
                model,claim['source_history'],claim['source_principal'])!=claim['membership_revision']:
            raise VqroError('Pi native model/history/shared runtime changed','stale')
        require_fresh('pi',runtime['argv'])
        native_argv=[*provider_argv(runtime['argv'],target),'--session',claim['source_history'],'--model',model['id']]
        if isinstance(model.get('effort'),str):native_argv+=['--thinking',model['effort']]
        hosts=self.ext.vqro.call('resource.list',{'id':claim['host']}).get('resources',[])
        if len(hosts)!=1 or not isinstance(hosts[0].get('provider'),str):
            raise VqroError('original execution backend is unavailable; no relocation','unavailable')
        with self.ext.cache_lock:backend=dict(self.ext.providers.get(hosts[0]['provider']) or {})
        spawn=(backend.get('capabilities') or {}).get('terminal.spawn')
        if backend.get('connected') is not True:
            raise VqroError('original execution backend is disconnected; nothing may be spawned','unavailable')
        if not isinstance(spawn,dict) or spawn.get('env') is False:
            raise VqroError('original backend cannot spawn a bridge with its required environment','unsupported')
        env=account_environment(target,runtime_environment('pi',runtime))
        env['VQRO_PRINCIPAL']=claim['source_principal']
        params={'bridge':True,'provider':hosts[0]['provider'],'name':'pi-resume-'+uuid.uuid4().hex[:12],
                'argv':[*runtime.get('bridge_argv',default_bridge()),'--',*native_argv],'env':env}
        cwd=session.get('cwd') or source.get('cwd')
        if isinstance(cwd,str) and cwd:params['cwd']=cwd
        admission=self.ext.invoke('accounts.claim.resume.validate',{'claim':args['_claim'],**({'recovery':True} if recovery else {})})
        if not isinstance(admission,dict) or admission.get('validated') is not True:
            raise VqroError('protected Pi resume admission is unconfirmed','unavailable')
        return params

    def prepare_resume(self,args):
        self._resume_parameters(args,require_exit=False)
        return {'prepared':True}

    def resume(self,args):
        """Finish one admitted restart only after confirmed graceful old exit."""
        return self._launch_resume(args,recovery=False)

    def recover_resume(self,args):
        return self._launch_resume(args,recovery=True)

    def _launch_resume(self,args,*,recovery):
        params=self._resume_parameters(args,require_exit=True,recovery=recovery)
        consumed=self.ext.invoke('accounts.claim.resume.consume',{'claim':args['_claim'],**({'recovery':True} if recovery else {})})
        if not isinstance(consumed,dict) or consumed.get('consumed') is not True:
            raise VqroError('Pi resume launch consumption unconfirmed; do not launch','indeterminate')
        try:
            result=launch(self.ext,params,brand='Pi resume',initialize_route=self.initialize_request_route,
                          show_terminal=self.terminal,expected_agents=1)
        except Exception:
            self.ext.invoke('accounts.claim.resume.result',{'claim':args['_claim'],'observed':False})
            raise
        observed=isinstance(result,dict) and result.get('outcome')=='observed' and isinstance(result.get('agent'),str)
        self.ext.invoke('accounts.claim.resume.result',{'claim':args['_claim'],'observed':observed})
        return result

    def initialize_request_route(self, agent):
        # Preserve Pi's return shape while sharing the actual-caller default mechanism.
        initialize_request_route(self.ext, agent, brand="Pi")

    def terminal(self, host):
        try:
            return self.ext.invoke("tabs.show", {"resource": host})
        except VqroError:
            self.ext.notify("Terminal "+host+" still exists; tabs unavailable. Use the resource picker.", "warn")

    def settings(self, args):
        self.ext.fresh()
        value = self.ext.value(SETTINGS, DEFAULTS)
        self.ext.publish("pi-settings", {"title": "Pi runtime argv", "requires": ["input"], "items": [
            {"type": "text", "text": "Literal Pi argv; optional bridge_argv and bridge_env resolve on the terminal host."},
            {"type": "text", "text": label(json.dumps(value.get("runtimes", value) if isinstance(value, dict) else value, ensure_ascii=False), 8192)},
            {"text": "Edit Pi runtimes JSON list", "input": "JSON list of {name, argv}", "command": "pi.configure"}]})
        return {"client": {"popup": {"view": "pi-settings", "width": 90}}}

    def configure(self, args):
        try:
            value = self.ext.value(SETTINGS, {})
            value = dict(value) if isinstance(value, dict) else {}
            value["runtimes"] = json.loads(args.get("text", ""))
        except (ValueError, TypeError):
            raise VqroError("enter a JSON list of {name, argv}", "invalid") from None
        runtimes(value)
        def save():
            self.ext.apply([{"op": "set", "key": SETTINGS, "value": value, "expect": self.ext.version(SETTINGS)}], "pi: runtimes")
        self.ext.retrying(save)
        self.ext.notify("Pi runtimes saved; existing agents unchanged.", "ok")
        return {"client": {"close": True}}


def main():
    ext = Extension("pi", event_types=["entries", "resource"])
    Pi(ext)
    ext.run()


if __name__ == "__main__":
    main()
