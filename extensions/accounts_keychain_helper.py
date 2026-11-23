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
import sys
import uuid
import warnings

from accounts_keychain import KeychainError, Secret, backend, valid_handle

PROVIDER_ENV = {'openai': 'OPENAI_API_KEY', 'anthropic': 'ANTHROPIC_API_KEY'}
CONFLICTING_ENV = frozenset((*PROVIDER_ENV.values(), 'ANTHROPIC_AUTH_TOKEN', 'CLAUDE_CODE_OAUTH_TOKEN'))


def launch(handle, provider, argv, *, store, environment=None, execute=os.execvpe):
    valid_handle(handle)
    if provider not in PROVIDER_ENV:
        raise KeychainError('unsupported')
    if not isinstance(argv, list) or not 1 <= len(argv) <= 128 or any(
            not isinstance(arg, str) or not arg or '\0' in arg for arg in argv):
        raise KeychainError('invalid_launch')
    env = dict(os.environ if environment is None else environment)
    for key in CONFLICTING_ENV:
        env.pop(key, None)
    secret = store.retrieve(handle)
    env[PROVIDER_ENV[provider]] = secret.reveal()
    try:
        execute(argv[0], argv, env)  # exactly once; no shell exports or core env RPC
    except OSError:
        raise KeychainError('unavailable') from None
    finally:
        env.pop(PROVIDER_ENV[provider], None)


def pending_store(handle):
    # Flush the non-secret recovery reference BEFORE any mutation can begin.
    print(json.dumps({'status': 'pending_store', 'handle': handle}), file=sys.stderr, flush=True)


def enter(*, store, read=getpass.getpass, terminal=None, announce=pending_store):
    if terminal is None:
        terminal = sys.stdin.isatty() and sys.stderr.isatty()
    if not terminal:
        raise KeychainError('denied')
    try:
        with warnings.catch_warnings():
            # getpass falls back to echoed stdin when it cannot disable echo.
            # Reject that fallback BEFORE it reads any input.
            warnings.simplefilter('error', getpass.GetPassWarning)
            secret = Secret(read('API key (hidden): '))
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
    commands.add_parser('store', help='Read an API key with hidden terminal input; print only its handle')
    delete = commands.add_parser('delete', help='Delete an explicitly selected handle')
    delete.add_argument('--handle', required=True)
    start = commands.add_parser('launch', help='Resolve a host-local handle and directly launch the runtime')
    start.add_argument('--handle', required=True)
    start.add_argument('--provider', required=True, choices=tuple(PROVIDER_ENV))
    start.add_argument('runtime', nargs=argparse.REMAINDER)
    args = parser.parse_args(argv)
    try:
        if args.action == 'launch':
            runtime = args.runtime[1:] if args.runtime[:1] == ['--'] else args.runtime
            launch(args.handle, args.provider, runtime, store=store_factory())
            return 0  # only reached with an injected exec in tests
        if args.action == 'store':
            print(json.dumps({'handle': enter(store=store_factory())}))
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
