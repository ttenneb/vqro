#!/usr/bin/env python3
"""Execution-host helper: hidden entry, explicit delete, env-only direct launch.

Invoke via the host's Python with an absolute helper path. No shell and no secret
arguments. This is not a credential service or an authorization boundary against
other programs running as the same OS user. See docs/research/accounts-keychain.md.
"""
import argparse
import getpass
import json
import os
import subprocess
import sys
import uuid
import warnings

from accounts_keychain import KeychainError, Secret, backend, valid_handle

PROVIDER_ENV = {'openai': 'OPENAI_API_KEY', 'anthropic': 'ANTHROPIC_API_KEY'}
CONFLICTING_ENV = frozenset((*PROVIDER_ENV.values(), 'ANTHROPIC_AUTH_TOKEN', 'CLAUDE_CODE_OAUTH_TOKEN'))
CREDENTIAL_KINDS = ('api_key', 'claude_subscription')


def credential_env(provider, credential_kind):
    if credential_kind == 'claude_subscription' and provider == 'anthropic':
        return 'CLAUDE_CODE_OAUTH_TOKEN'
    if credential_kind == 'api_key' and provider in PROVIDER_ENV:
        return PROVIDER_ENV[provider]
    raise KeychainError('unsupported')


def require_credential_launch_host(credential_kind):
    if credential_kind == 'claude_subscription' and os.name == 'nt':
        raise KeychainError('not supported on Windows yet')


def launch(handle, provider, argv, *, store, environment=None, execute=os.execvpe, credential_kind='api_key'):
    require_credential_launch_host(credential_kind)
    valid_handle(handle)
    target_env = credential_env(provider, credential_kind)
    if not isinstance(argv, list) or not 1 <= len(argv) <= 128 or any(
            not isinstance(arg, str) or not arg or '\0' in arg for arg in argv):
        raise KeychainError('invalid_launch')
    env = dict(os.environ if environment is None else environment)
    for key in CONFLICTING_ENV:
        env.pop(key, None)
    secret = store.retrieve(handle)
    env[target_env] = secret.reveal()
    try:
        # CRT exec joins raw elements on Windows; encode each argument exactly
        # once, preserving the explicit program and the original POSIX argv.
        encoded = [subprocess.list2cmdline([argument]) for argument in argv] if os.name == 'nt' else argv
        execute(argv[0], encoded, env)  # exactly once; no shell exports or core env RPC
    except OSError:
        raise KeychainError('unavailable') from None
    finally:
        env.pop(target_env, None)


def pending_store(handle):
    # Flush the non-secret recovery reference BEFORE any mutation can begin.
    print(json.dumps({'status': 'pending_store', 'handle': handle}), file=sys.stderr, flush=True)


def enter(*, store, read=getpass.getpass, terminal=None, announce=pending_store, credential_kind='api_key'):
    if credential_kind not in CREDENTIAL_KINDS:
        raise KeychainError('unsupported')
    if terminal is None:
        terminal = sys.stdin.isatty() and sys.stderr.isatty()
    if not terminal:
        raise KeychainError('denied')
    try:
        with warnings.catch_warnings():
            # getpass falls back to echoed stdin when it cannot disable echo.
            # Reject that fallback BEFORE it reads any input.
            warnings.simplefilter('error', getpass.GetPassWarning)
            prompt = 'Claude setup token (hidden): ' if credential_kind == 'claude_subscription' else 'API key (hidden): '
            secret = Secret(read(prompt))
        handle = uuid.uuid4().hex
        announce(handle)
        try:
            return store.store(secret, handle=handle)
        except KeychainError as error:
            raise KeychainError(error.status, handle=handle) from None
        except (Exception, KeyboardInterrupt):
            raise KeychainError('indeterminate', handle=handle) from None
    except getpass.GetPassWarning:
        raise KeychainError('denied') from None
    except EOFError:
        raise KeychainError('cancelled') from None


def main(argv=None, *, store_factory=backend):
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest='action', required=True)
    add = commands.add_parser('store', help='Read a credential with hidden terminal input; print only its handle')
    add.add_argument('--kind', choices=CREDENTIAL_KINDS, default='api_key')
    delete = commands.add_parser('delete', help='Delete an explicitly selected handle')
    delete.add_argument('--handle', required=True)
    check = commands.add_parser('check', help='Check host-local credential availability; never print the value')
    check.add_argument('--handle', required=True)
    check.add_argument('--provider', required=True, choices=tuple(PROVIDER_ENV))
    check.add_argument('--kind', choices=CREDENTIAL_KINDS, default='api_key')
    start = commands.add_parser('launch', help='Resolve a host-local handle and directly launch the runtime')
    start.add_argument('--handle', required=True)
    start.add_argument('--provider', required=True, choices=tuple(PROVIDER_ENV))
    start.add_argument('--kind', choices=CREDENTIAL_KINDS, default='api_key')
    start.add_argument('runtime', nargs=argparse.REMAINDER)
    args = parser.parse_args(argv)
    try:
        if args.action == 'launch':
            require_credential_launch_host(args.kind)
            runtime = args.runtime[1:] if args.runtime[:1] == ['--'] else args.runtime
            launch(args.handle, args.provider, runtime, store=store_factory(), credential_kind=args.kind)
            return 0  # only reached with an injected exec in tests
        if args.action == 'check':
            valid_handle(args.handle)
            credential_env(args.provider, args.kind)
            secret = store_factory().retrieve(args.handle)
            if not isinstance(secret, Secret):
                raise KeychainError('backend_error')
            del secret
            print(json.dumps({'status': 'ready'}))
        elif args.action == 'store':
            print(json.dumps({'handle': enter(store=store_factory(), credential_kind=args.kind)}))
        else:
            print(json.dumps({'status': store_factory().delete(args.handle)}))
        return 0
    except KeychainError as error:
        result = {'status': error.status}
        if error.handle is not None:
            result['handle'] = error.handle
        print(json.dumps(result), file=sys.stderr)
        return 1
    except KeyboardInterrupt:
        # A mutation may already have occurred; never claim cancellation rolled it back.
        print(json.dumps({'status': 'indeterminate'}), file=sys.stderr)
        return 1
    except Exception:
        # Never allow native errors or an exception repr containing a key to escape.
        print(json.dumps({'status': 'backend_error'}), file=sys.stderr)
        return 1


if __name__ == '__main__':
    sys.exit(main())
