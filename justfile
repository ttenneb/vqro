# vqro task runner
set windows-shell := ["cmd.exe", "/d", "/s", "/c"]

python := if os() == "windows" { "python" } else { "python3" }

# Run tests
test:
    cargo nextest run --locked --status-level fail --final-status-level fail --failure-output final --success-output never
    just maintenance-test
    just ui-hot-path-architecture-test
    just integration-assets-test
    just pi-launch-profile-test
    just pi-reply-routing-test
    just task-core-test
    just docs-contract-test

# Run repository maintenance contract tests
maintenance-test:
    {{python}} -m unittest scripts.test_agent_detection_manifest_check scripts.test_changelog scripts.test_config_reference_check scripts.test_docs_translation_parity scripts.test_hermes_integration_asset scripts.test_package_windows_conpty scripts.test_preview scripts.test_unix_installer scripts.test_vendor_libghostty_vt scripts.test_vendor_portable_pty scripts.test_windows_cross

# Characterize the trusted Tasks extension's store, replay, and task policy.
task-core-test:
    {{python}} -B -m unittest discover -s extensions/tasks -p '*_test.py'

# Readiness: one-command private launch installs the bundled defaults and two
# model-free Pi panes run import -> delegate -> badge -> decision -> message.
# Needs a built vqro and a pi on PATH; writes only under a new private root.
readiness-demo:
    python3 extensions/tasks/demo_readiness.py --vqro "${CARGO_TARGET_DIR:-target}/debug/vqro" --pi "$(command -v pi)"

# Check that a Pi Vqro starts gets the Vqro-only profile (model-free).
pi-profile-demo:
    python3 extensions/tasks/demo_pi_profile.py --vqro "${CARGO_TARGET_DIR:-target}/debug/vqro" --pi "$(command -v pi)"

# Run one nextest filter, e.g. `just test-one codex_stale_working`
test-one filter:
    cargo nextest run --locked "{{filter}}" --status-level fail --final-status-level fail --failure-output final --success-output never

# Verify, install, execute, and structurally audit the exact pinned Collections R0 artifact.
# Requires a clean checkout at VQRO_COLLECTIONS_CHECKOUT; no production catalog is changed.
collections-package-conformance:
    cargo nextest run --locked 'collections_candidate_passes_authoritative_host_gate' --run-ignored ignored-only --status-level fail --final-status-level fail --failure-output final --success-output never

# Verify and invoke the exact pinned zero-capability Spaces package artifact.
spaces-package-conformance:
    cargo nextest run --locked 'spaces_package_passes_authoritative_host_gate' --run-ignored ignored-only --status-level fail --final-status-level fail --failure-output final --success-output never

# Enforce deterministic UI hot-path architecture boundaries
ui-hot-path-architecture-test:
    {{python}} -m unittest scripts.test_ui_hot_path_architecture

# Run fast local lint checks
[unix]
lint:
    cargo fmt --check
    cargo clippy --all-targets --locked -- -D warnings

[script("powershell.exe", "-NoProfile", "-ExecutionPolicy", "Bypass", "-File")]
[windows]
lint:
    & .\scripts\windows_check.ps1 -Mode lint

# Run PR CI checks
ci filter='all()': lint
    just ci-tests "{{filter}}"

# Keep the test build independently configurable from clippy in CI.
ci-tests filter='all()':
    cargo nextest run --locked -E "{{filter}}" --status-level fail --final-status-level slow --failure-output final --success-output never
    just maintenance-test
    just ui-hot-path-architecture-test
    just integration-assets-test
    just pi-launch-profile-test
    just pi-reply-routing-test
    just task-core-test

# Test the public-CLI Pi launch adapter without starting a server or model.
pi-launch-profile-test:
    node --test extensions/pi-launch-profile/launch.test.mjs

# Test the Pi reply-routing adapter without a server or model.
pi-reply-routing-test:
    bun test extensions/vqro-pi/index.test.ts extensions/vqro-pi/input-box.test.ts extensions/tasks/pi.test.ts

# Download the Windows SDK once (requires xwin; prompts for Microsoft's SDK license)
[unix]
setup-windows-cross *args:
    {{python}} scripts/windows_cross.py setup {{args}}

# Run Windows target lint from Unix/macOS to catch cfg(windows) compile and clippy failures before CI
[unix]
windows-lint:
    {{python}} scripts/windows_cross.py lint

# Check formatting + run unit tests + Windows target lint + documentation contract tests
[unix]
check: ci windows-lint
    just docs-contract-test
    @echo "docs reminder: if this changes user-facing behavior, make sure the relevant release docs are updated or called out before release."

[script("powershell.exe", "-NoProfile", "-ExecutionPolicy", "Bypass", "-File")]
[windows]
check:
    & .\scripts\windows_check.ps1 -Mode check

# Install repo-local git hooks
install-hooks:
    git config core.hooksPath .githooks
    chmod +x .githooks/pre-commit
    chmod +x .githooks/commit-msg
    @echo "installed git hooks from .githooks"

# Build release binary
build:
    cargo build --release --locked

# Non-gating full-render scaling profile for background workspaces and active panes
bench-render-scale:
    cargo test --release --locked --bin vqro render_scale_profile -- --ignored --nocapture --test-threads=1

<<<<<<< HEAD
# Profile full and retained text, static-image, and unchanged-image updates
bench-retained-graphics:
    cargo test --release --locked --bin vqro render_scale_profile_retained_graphics -- --ignored --nocapture --test-threads=1
=======
# Native Windows-only idle process-inspection profile; no server is started.
[windows]
bench-process-inspection:
    cargo test --release --locked --bin vqro windows_process_inspection_profile -- --ignored --nocapture --test-threads=1
>>>>>>> 8291a65c7a0102bd7a454f4d32979a2b31369945

# ~3-5 minute CPU comparison; downloads stable unless HERDR_PERF_BASELINE_BIN is set
bench-release-smoke:
    cargo build --release --locked
    scripts/release_perf_smoke.sh "${CARGO_TARGET_DIR:-target}/release/vqro"

# Test public documentation snapshot and release lifecycle tooling
docs-contract-test:
    bun test ./scripts/docs

# Test public trusted semantic provider examples without a server or model.
public-semantic-documents-test:
    {{python}} extensions/public-facts.test.py
    bun test extensions/public-activity/pi-role-status.test.ts

# Test bundled agent integration assets
integration-assets-test: public-semantic-documents-test
    bun test src/integration/assets/herdr-agent-state.test.ts
    bun test src/integration/assets/opencode/herdr-agent-state.test.ts
    bun test src/integration/assets/opencode/herdr-tui-session.test.ts

# Regenerate the C API bindings with bindgen-cli 0.72.1
libghostty-bindings *clang_args:
    bash scripts/generate_libghostty_bindings.sh {{clang_args}}

# Build the vendored libghostty-vt source dist
build-libghostty-vt:
    scripts/build_vendored_libghostty_vt.sh

# Check that release docs and changelog have been finalized from docs/next before release
release-docs-check:
    python3 scripts/agent_detection_manifest_check.py --require-all-published
    python3 scripts/config_reference_check.py
    node scripts/docs/versions.mjs check
    node scripts/docs/preview.mjs check
    just docs-contract-test
    @test -f docs/next/README.md
    @test -f docs/next/README.zh-CN.md
    @if ! diff -u CHANGELOG.md docs/next/CHANGELOG.md; then \
        echo "error: CHANGELOG.md differs from docs/next/CHANGELOG.md; finalize release notes before releasing"; \
        exit 1; \
    fi
    @for file in CONFIGURATION.md INTEGRATIONS.md SOCKET_API.md; do \
        if [ -e "$file" ]; then \
            echo "error: $file was replaced by technical docs; remove the root copy"; \
            exit 1; \
        fi; \
    done
    @test -d docs/next/website/src/content/docs
    @for file in docs/next/website/src/content/docs/*.mdx; do \
        for locale in ja zh-cn; do \
            translated="docs/next/website/src/content/docs/$locale/$(basename "$file")"; \
            if [ ! -f "$translated" ]; then \
                echo "error: $translated is missing; translate next docs before releasing"; \
                exit 1; \
            fi; \
        done; \
    done
    @for file in docs/next/website/src/content/docs/ja/*.mdx docs/next/website/src/content/docs/zh-cn/*.mdx; do \
        staged="docs/next/website/src/content/docs/$(basename "$file")"; \
        if [ ! -f "$staged" ]; then \
            echo "error: $file has no matching english doc; remove the stale translation"; \
            exit 1; \
        fi; \
    done
    python3 scripts/docs_translation_parity.py --docs-root docs/next/website/src/content/docs

# Validate release docs, render scaling, and end-to-end CPU before release preparation
pre-release-check:
    just release-docs-check
    just bench-render-scale
    just bench-release-smoke
    @echo "release review required: investigate material render-scaling regressions before publishing."
    @echo "release review required: update skills/herdr/SKILL.md for this stable release so it matches the current CLI, IDs, agent lifecycle semantics, and safety guidance."
    @echo "release policy: do not update skills/herdr/SKILL.md between stable releases; preview builds keep the latest stable skill."

# Prepare the release commit without tagging or pushing (usage: just release-prepare 0.1.1)
release-prepare version:
    @printf '%s\n' '{{version}}' | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+$' || { \
        echo "error: version must look like 0.6.6 without a v prefix"; \
        exit 1; \
    }
    @if ! git diff --quiet -- . ':(exclude)skills/herdr/SKILL.md' || \
        ! git diff --cached --quiet -- . ':(exclude)skills/herdr/SKILL.md' || \
        [ -n "$(git ls-files --others --exclude-standard)" ]; then \
        echo "error: commit all changes except skills/herdr/SKILL.md first"; \
        exit 1; \
    fi
    @git fetch origin master --tags
    @if git rev-parse "v{{version}}" >/dev/null 2>&1; then \
        echo "error: tag v{{version}} already exists"; \
        exit 1; \
    fi
    just pre-release-check
    python3 scripts/changelog.py prepare --version {{version}}
    cp CHANGELOG.md docs/next/CHANGELOG.md
    sed -i.bak 's/^version = ".*"/version = "{{version}}"/' Cargo.toml && rm -f Cargo.toml.bak
    cargo update -p herdr --offline
    just check
    git add CHANGELOG.md docs/next/CHANGELOG.md Cargo.toml Cargo.lock skills/herdr/SKILL.md
    git diff --cached --quiet || git commit -m "release: v{{version}}"
    @echo "v{{version}} release commit prepared. Review it, then run: just release-publish {{version}}"

# Tag and push an already-prepared release commit (usage: just release-publish 0.1.1)
release-publish version:
    @printf '%s\n' '{{version}}' | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+$' || { \
        echo "error: version must look like 0.6.6 without a v prefix"; \
        exit 1; \
    }
    @if [ -n "$(git status --porcelain)" ]; then \
        echo "error: working tree must be clean before publishing"; \
        exit 1; \
    fi
    @branch="$(git branch --show-current)"; \
    if [ "$branch" != "master" ]; then \
        echo "error: release-publish must run from master, got $branch"; \
        exit 1; \
    fi
    @git fetch origin master --tags
    @if git rev-parse "v{{version}}" >/dev/null 2>&1; then \
        echo "error: tag v{{version}} already exists"; \
        exit 1; \
    fi
    @cargo_version="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"; \
    if [ "$cargo_version" != "{{version}}" ]; then \
        echo "error: Cargo.toml version $cargo_version does not match {{version}}"; \
        exit 1; \
    fi
    just release-docs-check
    python3 scripts/changelog.py extract --version {{version}} --output /tmp/herdr-release-notes-check.md
    rm -f /tmp/herdr-release-notes-check.md
    @local_head="$(git rev-parse HEAD)"; \
    remote_head="$(git rev-parse origin/master)"; \
    if ! git merge-base --is-ancestor "$remote_head" "$local_head"; then \
        echo "error: origin/master is not an ancestor of HEAD; pull or rebase before publishing"; \
        exit 1; \
    fi; \
    if [ "$local_head" != "$remote_head" ]; then \
        echo "pushing release commit to origin/master"; \
        git push origin HEAD:master; \
    fi
    git tag -a v{{version}} -m "v{{version}}"
    git push origin v{{version}}
    @echo "v{{version}} released — GitHub Actions building binaries and updating distribution/latest.json"

# Prepare, verify, tag, push, and trigger the GitHub Release workflow (usage: just release 0.1.1)
release version:
    just release-prepare {{version}}
    just release-publish {{version}}

# Print default config
default-config:
    cargo run --release --locked -- --default-config
