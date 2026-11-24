"""Credential-only account selection shared by launchers; no credential I/O.
The registry holds references only. Native Pi owns auth-file reads, login and refresh.
An account never selects a configuration directory, skills, plugins or history.
"""
import hashlib
import json
from pathlib import PurePosixPath, PureWindowsPath
import re
from vqro_ext import VqroError
from vqro_display import label

SETTINGS = 'doc/settings/accounts'
SELECTORS = {'pi': 'PI_CODING_AGENT_DIR', 'codex': 'CODEX_HOME', 'claude': 'CLAUDE_CONFIG_DIR'}
MAX_ACCOUNTS = 64


def validate(row):
    if not isinstance(row, dict) or set(row) - {'id','label','runtime','provider','kind','native_provider','login_ref','secret_ref','host'}:
        raise VqroError('account records contain credential references and labels only; never profiles or tokens', 'invalid')
    if not isinstance(row.get('id'), str) or not re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9_.-]{0,63}', row['id']) or row['id'] == 'normal':
        raise VqroError('account id must be 1–64 safe characters; normal is reserved', 'invalid')
    if not isinstance(row.get('label'), str) or not row['label'].strip() or len(row['label']) > 128 or not row['label'].isprintable():
        raise VqroError('account label must be 1–128 printable characters', 'invalid')
    if row.get('runtime') not in SELECTORS or row.get('kind') not in ('subscription','api_key'):
        raise VqroError('account runtime or billing kind is unsupported', 'invalid')
    provider = row.get('provider')
    if 'provider' in row and (not isinstance(provider,str) or not re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9_.-]{0,63}',provider)):
        raise VqroError('provider must be a short provider identifier', 'invalid')
    if 'native_provider' in row and (not isinstance(row['native_provider'],str)
            or not re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9_.-]{0,63}',row['native_provider'])):
        raise VqroError('native_provider must be a short native provider identifier', 'invalid')
    if 'login_ref' in row:
        ref = row['login_ref']
        if not isinstance(ref,dict) or set(ref) != {'sessionId','accountId'} or any(
                not isinstance(value,str) or not value or not value.isprintable() or len(value.encode())>256 for value in ref.values()):
            raise VqroError('Codex login_ref contains only opaque native sessionId and accountId', 'invalid')
    if 'secret_ref' in row and (not isinstance(row['secret_ref'],str) or not re.fullmatch(r'[0-9a-f]{32}',row['secret_ref'])):
        raise VqroError('credential reference must be an opaque host-keychain handle, never a token or key', 'invalid')
    if 'host' in row and (not isinstance(row['host'],str) or not row['host'] or len(row['host']) > 128 or not row['host'].isprintable()):
        raise VqroError('credential host reference is invalid', 'invalid')
    return dict(row)


def selection_problem(row):
    """Safe metadata may remain visible while its credential route is incomplete."""
    routes = {key for key in ('native_provider', 'login_ref', 'secret_ref') if key in row}
    if len(routes) > 1:
        return 'Conflicting credential routes; keep exactly one native reference form.'
    if ('host' in row) != ('secret_ref' in row):
        return 'Host-keychain references require both host and secret_ref.'
    if row['kind'] == 'api_key':
        if routes != {'secret_ref'}:
            return 'API-key account needs a host-keychain reference.'
        if row.get('provider') not in ('openai', 'anthropic'):
            return 'API-key helper needs an explicit supported provider (openai or anthropic).'
        return None
    expected = {'pi': 'native_provider', 'codex': 'login_ref', 'claude': 'secret_ref'}[row['runtime']]
    if routes != {expected}:
        return 'Native subscription reference is missing or belongs to another runtime.'
    if row['runtime'] == 'pi' and not row.get('provider'):
        return 'Pi native provider aliases require an explicit base provider.'
    if row['runtime'] == 'pi' and row.get('native_provider') == row.get('provider'):
        return 'Pi account alias must not overwrite the base native provider.'
    return None


def credential_identity(row, *, legacy_provider=False):
    """Known reference equality only; never an authenticated-account assertion.

    Keep real runtime/issuer/kind/host namespaces distinct. Codex subscription
    saved logins have a fixed native issuer; configured provider spelling cannot
    split one login. Never inspect secrets or equate different opaque references.
    """
    if selection_problem(row):
        return None
    if 'native_provider' in row:
        route = ('native_provider', row['native_provider'])
    elif 'login_ref' in row:
        route = ('login_ref', row['login_ref']['sessionId'], row['login_ref']['accountId'])
    else:
        route = ('secret_ref', row['secret_ref'])
    # Native saved Codex logins and Claude subscriptions have an implicit
    # issuer; omission is not a different credential namespace.
    provider = row.get('provider')
    if row['kind']=='subscription' and row['runtime']=='codex' and not legacy_provider:
        # Native selection uses only sessionId/accountId, not this metadata.
        provider='openai'
    elif provider is None and row['kind'] == 'subscription':
        provider = {'codex':'openai','claude': 'anthropic'}.get(row['runtime'])
    return (row['runtime'], provider, row['kind'], row.get('host'), *route)


def distinct_accounts(rows):
    result, seen = [], set()
    for row in rows:
        identity = credential_identity(row)
        if identity is not None and identity not in seen:
            seen.add(identity)
            result.append(row)
    return result


def eligible_alternatives(rows, source_id, blocked_ids, allow_api_key):
    """Aliases are one choice; a blocked/source reference excludes ALL aliases."""
    by_id = {row['id']: row for row in rows}
    source = by_id.get(source_id)
    source_identity = credential_identity(source) if source else None
    blocked = {credential_identity(by_id[ident]) for ident in blocked_ids if ident in by_id}
    candidates = []
    for row in rows:
        identity = credential_identity(row)
        if identity is None or row['id'] == source_id or identity == source_identity or identity in blocked:
            continue
        if row['kind'] == 'api_key' and not allow_api_key:
            continue
        candidates.append(row)
    return distinct_accounts(candidates)


def native_registration(row):
    """Allocate a stable nonsecret namespace; native Pi owns its login entry.

    This never imports an old login. A migrated account requires native sign-in
    under its alias; profiles and credential files remain untouched.
    """
    row = dict(row)
    if row['runtime']=='pi' and row['kind']=='subscription' and row.get('provider') and not row.get('native_provider'):
        row['native_provider'] = 'vqro-account-'+hashlib.sha256(row['id'].encode()).hexdigest()[:32]
    return row


def migrate(ext):
    """Change registry metadata only. Never inspect/copy/delete native folders or tokens."""
    entry = ext.vqro.call('workspace.get', {'key': SETTINGS})
    if not entry:
        return
    value = entry['value']
    if not isinstance(value,dict) or set(value) - {'accounts'} or not isinstance(value.get('accounts'),list) or len(value['accounts']) > MAX_ACCOUNTS:
        raise VqroError('invalid account metadata; no migration performed', 'invalid')
    rows = []
    changed = False
    for original in value['accounts']:
        row = dict(original) if isinstance(original,dict) else original
        if isinstance(row,dict) and 'profile' in row:
            path = row.pop('profile')
            if not isinstance(path,str) or not (PurePosixPath(path).is_absolute() or PureWindowsPath(path).is_absolute()):
                raise VqroError('invalid legacy account path; no migration performed', 'invalid')
            row = native_registration(validate(row))
            changed = True
        rows.append(validate(row))
    if len({row['id'] for row in rows}) != len(rows):
        raise VqroError('account ids must be unique', 'invalid')
    if changed:
        ext.apply([{'op':'set','key':SETTINGS,'expect':entry['version'],'value':{'accounts':rows}}],
                  'accounts: remove configuration-folder linkage; metadata only')


def snapshot(ext):
    entry = ext.vqro.call('workspace.get', {'key': SETTINGS})
    value = entry['value'] if entry else {'accounts': []}
    if not isinstance(value,dict) or set(value) - {'accounts'} or not isinstance(value.get('accounts'),list) or len(value['accounts']) > MAX_ACCOUNTS:
        raise VqroError('accounts registry must contain at most 64 metadata-only accounts', 'invalid')
    rows = [validate(row) for row in value['accounts']]
    if len({r['id'] for r in rows}) != len(rows):
        raise VqroError('account ids must be unique', 'invalid')
    return rows, entry['version'] if entry else 0


def registry(ext):
    return snapshot(ext)[0]


def fact(account):
    fields = {k: account[k] for k in ('id','label','provider','kind') if k in account}
    return dict(fields, sources={k:'vqro:launcher' for k in fields})


def runtime_environment(runtime, row):
    """The runtime's shared environment, independent of account selection."""
    result = dict(row.get('bridge_env', {}))
    if runtime == 'codex':
        for field, key in (('codex_home','CODEX_HOME'), ('home','HOME')):
            if field in row: result[key] = row[field]
    return result


def login_environment(runtime, row):
    """Same shared setup as launch; never forward credentials or bridge supervision."""
    env = runtime_environment(runtime, row)
    keys = {'HOME','USERPROFILE','HOMEDRIVE','HOMEPATH','XDG_CONFIG_HOME','XDG_DATA_HOME','XDG_STATE_HOME',SELECTORS[runtime]}
    return {key:value for key,value in env.items() if key in keys}


def native_login_argv(runtime, argv):
    """Recognize inert native CLI options, never convert a prompt-bearing argv into sign-in.
    Unknown wrapper/option shapes refuse before spawn rather than risk a model turn.
    """
    prefix = [argv[0]]
    rest = list(argv[1:])
    binary = argv[0].replace('\\','/').rsplit('/',1)[-1].lower().removesuffix('.exe')
    if binary in ('node','nodejs','bun'):
        if not rest or rest[0].startswith('-') or not rest[0].endswith(('.js','.mjs','.cjs','.ts')):
            raise VqroError('native sign-in needs a recognized executable/script prefix','unsupported')
        prefix.append(rest.pop(0))
    if runtime == 'codex':
        if 'app-server' not in rest:
            raise VqroError('Codex sign-in needs a runtime argv containing app-server','unsupported')
        at = rest.index('app-server')
        # Validate the ENTIRE argv before converting it: only proved transport-only suffixes may disappear.
        # Global selector placement after the subcommand is not supported here; refuse rather than drop it.
        if rest[at+1:] not in ([], ['--stdio'], ['--listen','stdio://']):
            raise VqroError('Codex sign-in refuses unknown app-server suffixes; put profile selectors before app-server','unsupported')
        rest = rest[:at]
    values = {'pi': {'--provider','--model','--thinking'},
              'codex': {'-c','--config','-p','--profile','--enable','--disable'},
              'claude': {'--model','--effort','--settings','--setting-sources'}}[runtime]
    flags = {'pi': {'--no-extensions','--no-skills','--no-prompt-templates','--no-session','--no-tools'},
             'codex': set(), 'claude': {'--disable-slash-commands'}}[runtime]
    out = list(prefix); i = 0
    while i < len(rest):
        arg = rest[i]
        if runtime == 'pi' and arg in ('--mode','--mode=rpc'):
            if arg == '--mode':
                if i+1 >= len(rest) or rest[i+1] != 'rpc':
                    raise VqroError('Pi sign-in only converts native rpc mode to interactive','unsupported')
                i += 1
        elif arg in flags: out.append(arg)
        elif arg in values:
            if i+1 >= len(rest) or rest[i+1].startswith('-'):
                raise VqroError('native sign-in option is missing its value','unsupported')
            out.extend(rest[i:i+2]); i += 1
        elif any(arg.startswith(option+'=') and arg[len(option)+1:] for option in values if option.startswith('--')):
            out.append(arg)
        else:
            raise VqroError('native sign-in refuses prompts, noninteractive or unrecognized runtime options','unsupported')
        i += 1
    return out + (['login'] if runtime == 'codex' else ['auth','login'] if runtime == 'claude' else [])


def require_fresh(runtime, argv):
    """Fresh-work launches must not execute/resume a configured prompt or conversation."""
    try: native_login_argv(runtime, argv)  # strict inert-options/script-prefix validator, never executed
    except VqroError:
        raise VqroError('fresh work refuses resume, prompt-bearing or unrecognized native argv','unsupported') from None
    if runtime == 'codex':
        tail = argv[argv.index('app-server')+1:]
        if tail not in ([], ['--stdio'], ['--listen','stdio://']):
            raise VqroError('fresh Codex work requires a plain stdio app-server launch','unsupported')


def require_launch_binding(account):
    if account and account['kind']=='api_key':
        raise VqroError('API-key accounts: launching is not available yet; the keychain entry is kept','unsupported')


def environment(account, env=None):
    require_launch_binding(account)
    result = dict(env or {})
    # Untrusted runtime settings cannot smuggle a different account fact into a normal launch.
    result.pop('VQRO_ACCOUNT', None)
    result.pop('VQRO_PI_AUTH_FILE', None)
    result.pop('VQRO_PI_ACCOUNT', None)
    if account:
        if account['runtime'] == 'pi' and account.get('native_provider'):
            result['VQRO_PI_ACCOUNT'] = json.dumps({key:account[key] for key in ('id','label','provider','native_provider')},
                                                  ensure_ascii=False,separators=(',',':'))
        result['VQRO_ACCOUNT'] = json.dumps(fact(account), ensure_ascii=False, separators=(',',':'))
    return result


def choose(ext, args, runtime, command):
    rows, revision = snapshot(ext)
    selected = args.get('account')
    if '_account_revision' in args and (type(args['_account_revision']) is not int or args['_account_revision'] != revision):
        raise VqroError('account choices changed; choose again before launching', 'stale')
    registered = [row for row in rows if row['runtime'] == runtime]
    candidates = [row for row in registered if selection_problem(row) is None]
    if selected == 'normal': return None, None
    if selected is None and not registered:
        from accounts_policy import settings
        if settings(ext)['new_agents'] == 'ask': return None, None
        # Explicit capacity/rotation policy with no candidate must ask, not guess normal-login capacity.
    if selected is not None:
        account = next((row for row in candidates if row['id'] == selected), None)
        if account is None:
            invalid = next((row for row in registered if row['id'] == selected), None)
            reason = selection_problem(invalid) if invalid else 'Account is missing or belongs to another program.'
            raise VqroError('account is not selectable: ' + reason, 'invalid')
        return account, None
    if selected is None:
        from accounts_policy import pick_new
        account = pick_new(ext, candidates, runtime,
                           credential_identities={row['id']: credential_identity(row) for row in rows})
        if account is not None: return account, None
    view = 'accounts-choice-' + command.replace('.', '-')
    forwarded = {k:args[k] for k in ('runtime','cwd','context','fresh') if k in args}
    primary = {credential_identity(row): row['id'] for row in reversed(distinct_accounts(candidates))}
    ext.publish(view, {'title':'New '+runtime+' agent: account', 'requires':['menu'], 'items':[
        {'text':'Shared runtime login', 'description':'One shared setup; native login identity is not inspected', 'command':command,
         'args':dict(forwarded, account='normal', _account_revision=revision)},
        *[{'text':label(row['label']), 'description':label(row.get('provider',runtime)+' · '+row['kind']), 'command':command,
           'args':dict(forwarded, account=row['id'], _account_revision=revision)} for row in distinct_accounts(candidates)],
        *[{'type':'text','style':'dim','text':label(row['label'])+' - alias of '+primary[credential_identity(row)]+' (same credential reference)'}
          for row in candidates if primary[credential_identity(row)] != row['id']],
        *[{'type':'text','style':'warn','text':label(row['label'])+' - invalid: '+selection_problem(row)}
          for row in registered if selection_problem(row) is not None]]})
    return None, {'client':{'popup':{'view':view,'width':72}}}


def provider_argv(argv, account):
    require_launch_binding(account)
    result = list(argv)
    if account and account['runtime'] == 'pi' and account.get('provider'):
        provider = account['provider']
        for i,arg in enumerate(result):
            if arg == '--provider' and (i+1 == len(result) or result[i+1] != provider) or arg.startswith('--provider=') and arg != '--provider='+provider:
                raise VqroError('runtime provider conflicts with selected account; nothing launched', 'invalid')
        if '--provider' not in result and not any(arg.startswith('--provider=') for arg in result):
            result += ['--provider',provider]
    return result
