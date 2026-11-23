"""Bundled account/profile policy shared by launchers; no native profile I/O.
The registry is ordinary workspace metadata. Secrets remain entirely runtime-owned.
"""
import json
from pathlib import PurePosixPath, PureWindowsPath
import re
from vqro_ext import VqroError
from vqro_display import label

SETTINGS = 'doc/settings/accounts'
SELECTORS = {'pi': 'PI_CODING_AGENT_DIR', 'codex': 'CODEX_HOME', 'claude': 'CLAUDE_CONFIG_DIR'}
MAX_ACCOUNTS = 64


def validate(row):
    if not isinstance(row, dict) or set(row) - {'id','label','runtime','provider','kind','profile'}:
        raise VqroError('account records contain only id, label, runtime, provider?, kind, profile; never credentials', 'invalid')
    if not isinstance(row.get('id'), str) or not re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9_.-]{0,63}', row['id']) or row['id'] == 'normal':
        raise VqroError('account id must be 1–64 safe characters; normal is reserved', 'invalid')
    if not isinstance(row.get('label'), str) or not row['label'].strip() or len(row['label']) > 128 or not row['label'].isprintable():
        raise VqroError('account label must be 1–128 printable characters', 'invalid')
    if row.get('runtime') not in SELECTORS or row.get('kind') not in ('subscription','api_key'):
        raise VqroError('account runtime or billing kind is unsupported', 'invalid')
    provider = row.get('provider')
    if 'provider' in row and (not isinstance(provider,str) or not re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9_.-]{0,63}',provider)):
        raise VqroError('provider must be a short provider identifier', 'invalid')
    path = row.get('profile')
    if not isinstance(path,str) or not path.strip() or len(path.encode()) > 4096 or '\0' in path or not path.isprintable() \
            or not (PurePosixPath(path).is_absolute() or PureWindowsPath(path).is_absolute()):
        raise VqroError('profile must be a literal absolute terminal-host path (no ~ expansion)', 'invalid')
    return dict(row)


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
    """The launcher's configured env, including explicit normal-profile selectors."""
    result = dict(row.get('bridge_env', {}))
    if runtime == 'codex':
        for field, key in (('codex_home','CODEX_HOME'), ('home','HOME')):
            if field in row: result[key] = row[field]
    return result


def login_environment(runtime, row):
    """Same profile namespace as launch; never forward credentials or bridge supervision."""
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


def environment(account, env=None):
    result = dict(env or {})
    # Untrusted runtime settings cannot smuggle a different account fact into a normal launch.
    result.pop('VQRO_ACCOUNT', None)
    if account:
        result[SELECTORS[account['runtime']]] = account['profile']
        result['VQRO_ACCOUNT'] = json.dumps(fact(account), ensure_ascii=False, separators=(',',':'))
    return result


def choose(ext, args, runtime, command):
    rows, revision = snapshot(ext)
    selected = args.get('account')
    if '_account_revision' in args and args['_account_revision'] != revision:
        raise VqroError('account choices changed; choose again before launching', 'stale')
    candidates = [row for row in rows if row['runtime'] == runtime]
    if selected == 'normal' or selected is None and not candidates:
        return None, None
    if selected is not None:
        account = next((row for row in candidates if row['id'] == selected), None)
        if account is None:
            raise VqroError('account is missing or belongs to another program; nothing launched', 'invalid')
        return account, None
    view = 'accounts-choice-' + command.replace('.', '-')
    forwarded = {k:args[k] for k in ('runtime','cwd','context') if k in args}
    ext.publish(view, {'title':'New '+runtime+' agent: account', 'requires':['menu'], 'items':[
        {'text':'Use my normal login', 'description':'Native defaults; login and billing mode not inspected', 'command':command,
         'args':dict(forwarded, account='normal', _account_revision=revision)},
        *[{'text':label(row['label']), 'description':label(row.get('provider',runtime)+' · '+row['kind']), 'command':command,
           'args':dict(forwarded, account=row['id'], _account_revision=revision)} for row in candidates]]})
    return None, {'client':{'popup':{'view':view,'width':72}}}


def provider_argv(argv, account):
    result = list(argv)
    if account and account['runtime'] == 'pi' and account.get('provider'):
        provider = account['provider']
        for i,arg in enumerate(result):
            if arg == '--provider' and (i+1 == len(result) or result[i+1] != provider) or arg.startswith('--provider=') and arg != '--provider='+provider:
                raise VqroError('runtime provider conflicts with selected account; nothing launched', 'invalid')
        if '--provider' not in result and not any(arg.startswith('--provider=') for arg in result):
            result += ['--provider',provider]
    return result
