"""Protected once-only cutover and NEW-turn intents, never credentials or history.

The accounts extension owns this journal. Runtime callbacks only validate its public
claim-read view and perform their guarded native restart; they never claim or send.
Unknown effects retain the setup fence. There is no private/undoable fallback.
"""
import copy
import json
import math
import time
import uuid

from accounts_policy import DOMAIN, MAX_ATTEMPTS, episode, facts, settings, configured_id, blocked, capacities
from vqro_ext import VqroError
from account_credentials import credential_identity, snapshot as account_snapshot, selection_problem
import hashlib

KEY = 'operational/accounts/cutovers'
MAX_BYTES = 48 * 1024
STATES = {'claimed', 'native_ready', 'unknown', 'completed', 'rejected', 'awaiting_owner', 'owner_turn'}
CHILD_STATES = {'claimed', 'observed', 'unknown', 'cancelled'}
MEMBER_FIELDS = {'resource', 'host', 'session', 'epoch', 'incarnation', 'account'}
RECORD_FIELDS = {'state', 'resource', 'episode', 'host', 'account', '_account_revision',
                 'automatic', 'setup_id', 'membership_revision', 'affected', 'runtime',
                 'source_incarnation', 'source_epoch', 'source_account', 'session', 'native_episode',
                 'continuation_text', 'continuations', 'authorization', 'created_at', 'inference', 'continuation_cancelled', 'source_history', 'source_principal', 'authorized_targets', 'attempted_targets', 'resumed_members', 'continuation_owner', 'member_episodes', 'continuation_parent', 'resume_launch'}


def text(value, bound=256):
    try:
        return isinstance(value, str) and 0 < len(value.encode('utf-8')) <= bound and value.isprintable()
    except UnicodeError:
        return False


def member(value):
    if not isinstance(value, dict) or set(value) != MEMBER_FIELDS or not all(text(item) for item in value.values()):
        raise VqroError('cutover membership is incomplete or unsupported', 'unavailable')
    return dict(value)


def credential_key(account):
    identity=credential_identity(account)
    return hashlib.sha256(json.dumps(identity,separators=(',',':')).encode()).hexdigest() if identity is not None else None


def limit_marker(resource):
    native=facts(resource);availability=native.get('availability')
    reported=availability.get('native') if isinstance(availability,dict) else None
    reported=reported if isinstance(reported,dict) else {}
    completion=native.get('completion');detail=completion.get('native') if isinstance(completion,dict) else None
    witness=reported.get('turnId') or reported.get('episode')
    if witness is None and isinstance(detail,dict):witness=detail.get('timestamp')
    return hashlib.sha256(json.dumps([(native.get('session') or {}).get('id'),witness if witness is not None else episode(resource)]).encode()).hexdigest()


def block_active(block,now):
    return now < block['reset_at'] if block['reset_at'] is not None else now < block['observed_at']+3600


def hourly_counts(claims, runtime, affected, now):
    """A group move charges every native conversation, independent of its leader."""
    counts = {item['session']: 0 for item in affected}
    for old in claims.values():
        if old['automatic'] and old['runtime'] == runtime and old['created_at'] >= now - 3600:
            for session in {item['session'] for item in old['affected']} & counts.keys():
                if old['continuation_parent'] is None:
                    counts[session] += len(old['authorized_targets']) if old['attempted_targets'] is None else len(old['attempted_targets'])
    return counts.values()


def continuation_text(value):
    try:
        return isinstance(value, str) and bool(value.strip()) and '\0' not in value and len(value.encode('utf-8')) <= 4096
    except UnicodeError:
        return False


def validate_claim_authority(ext, claim, automatic):
    """Callback verification only: never grant click authority or create a claim."""
    auth = claim.get('authorization') if isinstance(claim, dict) else None
    if automatic:
        if auth != {'mode': 'auto', 'principal': None, 'owner_revision': None}:
            raise VqroError('protected AUTO authorization is required', 'forbidden')
        return
    if not isinstance(auth, dict) or set(auth) not in ({'mode', 'principal', 'owner_revision'},
            {'mode', 'principal', 'owner_revision', 'background_loss_disclosed'}) \
            or ('background_loss_disclosed' in auth and (auth['background_loss_disclosed'] is not True or claim.get('runtime') != 'claude')) \
            or auth['mode'] != 'offer' or not text(auth['principal']) \
            or type(auth['owner_revision']) is not int or auth['owner_revision'] <= 0:
        raise VqroError('protected owner-click authorization is required', 'forbidden')
    entry = ext.vqro.call('workspace.get', {'key': 'doc/principals/owner'})
    if not isinstance(entry, dict) or entry.get('value') != auth['principal'] \
            or type(entry.get('version')) is not int or entry['version'] != auth['owner_revision']:
        raise VqroError('recorded owner-click authority is no longer current', 'stale')


class Cutovers:
    def __init__(self, ext, clock=time.time):
        self.ext = ext
        self.registered = False
        self.clock = clock

    def authorization(self, automatic, *, background_loss_disclosed=False):
        if type(background_loss_disclosed) is not bool or automatic and background_loss_disclosed:
            raise VqroError('background-loss override requires a disclosed owner OFFER', 'forbidden')
        if automatic:
            return {'mode': 'auto', 'principal': None, 'owner_revision': None}
        caller = self.ext.caller() or {}
        self.ext.fresh()
        owner = self.ext.owner()
        entry = self.ext.vqro.call('workspace.get', {'key': 'doc/principals/owner'})
        if not owner or caller.get('principal') != owner or not isinstance(entry, dict) \
                or entry.get('value') != owner or type(entry.get('version')) is not int or entry['version'] <= 0:
            raise VqroError('explicit click by the current authenticated workspace owner is required', 'forbidden')
        authority = {'mode': 'offer', 'principal': owner, 'owner_revision': entry['version']}
        if background_loss_disclosed:
            authority['background_loss_disclosed'] = True
        return authority

    def read(self):
        if not self.registered:
            self.ext.vqro.call('operational.register', {'name': DOMAIN, 'bytes': 128 * 1024})
            self.registered = True
        reply = self.ext.vqro.call('operational.get', {'domain': DOMAIN, 'key': KEY})
        entry = reply.get('entry') if isinstance(reply, dict) else None
        if entry is None:
            return {'v': 1, 'claims': {}, 'groups': {}, 'blocks': {}}, 0
        value = entry.get('value')
        try:
            valid = (type(entry.get('version')) is int and entry['version'] > 0 and isinstance(value, dict)
                     and set(value) == {'v', 'claims', 'groups', 'blocks'} and type(value['v']) is int and value['v'] == 1
                     and isinstance(value['claims'], dict) and isinstance(value['groups'], dict)
                     and len(value['claims']) <= MAX_ATTEMPTS and len(value['groups']) <= MAX_ATTEMPTS)
            if not valid:
                raise ValueError()
            if not isinstance(value['blocks'],dict) or len(value['blocks'])>128:raise ValueError()
            for key,block in value['blocks'].items():
                if not text(key,64) or len(key)!=64 or any(c not in '0123456789abcdef' for c in key) \
                        or not isinstance(block,dict) or set(block)!={'observed_at','reset_at','episode'} or not text(block.get('episode'),128):
                    raise ValueError()
                for field in ('observed_at','reset_at'):
                    stamp=block[field]
                    if stamp is None and field=='reset_at':continue
                    if type(stamp) not in (int,float) or not math.isfinite(stamp) or stamp<0:raise ValueError()
            for token, record in value['claims'].items():
                if not text(token, 32) or len(token) != 32 or any(char not in '0123456789abcdef' for char in token):
                    raise ValueError()
                if not isinstance(record, dict) or set(record) != RECORD_FIELDS or record['state'] not in STATES:
                    raise ValueError()
                if not all(text(record[key]) for key in ('resource', 'episode', 'host', 'account', 'setup_id',
                        'membership_revision', 'runtime', 'source_incarnation', 'source_epoch', 'session')):
                    raise ValueError()
                launch=record['resume_launch']
                if launch is not None and (record['runtime']!='pi' or not isinstance(launch,dict)
                        or set(launch)!={'state','principal'} or launch['state'] not in ('claimed','observed','unknown')
                        or not text(launch['principal'])):raise ValueError()
                parent=record['continuation_parent']
                if parent is not None and (not text(parent,32) or parent not in value['claims']):raise ValueError()
                episodes=record['member_episodes']
                if not isinstance(episodes,list) or len(episodes)>MAX_ATTEMPTS:raise ValueError()
                for item in episodes:
                    if not isinstance(item,dict) or set(item)!={'resource','host','incarnation','epoch','session','account','native_episode','episode'} \
                            or not all(text(value) for value in item.values()):raise ValueError()
                targets=record['authorized_targets'];attempted=record['attempted_targets']
                if not isinstance(targets,list) or not 1<=len(targets)<=10 or len(set(targets))!=len(targets) \
                        or not all(text(target,64) for target in targets) or record['account'] not in targets:
                    raise ValueError()
                if attempted is not None and (not isinstance(attempted,list) or attempted!=targets[:len(attempted)]):
                    raise ValueError()
                if record['runtime']!='codex' and len(targets)!=1:raise ValueError()
                if record['runtime'] not in ('pi', 'codex', 'claude') or type(record['_account_revision']) is not int \
                        or record['_account_revision'] < 0 or type(record['automatic']) is not bool \
                        or not continuation_text(record['continuation_text']) or type(record['continuation_cancelled']) is not bool:
                    raise ValueError()
                if record['source_principal'] is not None and (record['runtime'] != 'pi' or not text(record['source_principal'],128)):
                    raise ValueError()
                if record['source_history'] is not None and (record['runtime'] != 'pi' or not text(record['source_history'], 4096)):
                    raise ValueError()
                if record['source_account'] is not None and not text(record['source_account'], 64):
                    raise ValueError()
                inference = record['inference']
                if inference is not None and (not isinstance(inference, dict)
                        or set(inference) != {'table_version', 'entry', 'label'}
                        or type(inference['table_version']) is not int or inference['table_version'] <= 0
                        or not text(inference['entry'], 128) or not text(inference['label'], 512)):
                    raise ValueError()
                auth = record['authorization']
                if not isinstance(auth, dict) or set(auth) not in ({'mode', 'principal', 'owner_revision'},
                        {'mode', 'principal', 'owner_revision', 'background_loss_disclosed'}):
                    raise ValueError()
                if 'background_loss_disclosed' in auth and (auth['background_loss_disclosed'] is not True
                        or record['automatic'] or record['runtime'] != 'claude'):
                    raise ValueError()
                if record['automatic']:
                    if auth != {'mode': 'auto', 'principal': None, 'owner_revision': None}:
                        raise ValueError()
                elif auth['mode'] != 'offer' or not text(auth['principal']) or type(auth['owner_revision']) is not int or auth['owner_revision'] <= 0:
                    raise ValueError()
                if type(record['created_at']) not in (int, float) or not math.isfinite(record['created_at']) or record['created_at'] < 0:
                    raise ValueError()
                if record['native_episode'] is not None and not text(record['native_episode']):
                    raise ValueError()
                if not isinstance(record['affected'], list) or not 1 <= len(record['affected']) <= MAX_ATTEMPTS:
                    raise ValueError()
                for item in record['affected']:
                    member(item)
                if record['continuation_owner'] is not None:
                    owner=record['continuation_owner']
                    if not isinstance(owner,dict) or set(owner)!={'mode','principal','owner_revision'} or owner['mode']!='offer' \
                            or not text(owner['principal']) or type(owner['owner_revision']) is not int or owner['owner_revision']<=0:
                        raise ValueError()
                if not isinstance(record['resumed_members'],list) or len(record['resumed_members'])>MAX_ATTEMPTS:raise ValueError()
                if record['state']=='owner_turn' and record['continuation_owner'] is None:raise ValueError()
                if record['state']=='awaiting_owner' and not record['resumed_members']:raise ValueError()
                for item in record['resumed_members']:member(item)
                if record['state'] not in ('completed', 'rejected', 'awaiting_owner', 'owner_turn') and value['groups'].get(self.group(record)) != token:
                    raise ValueError()
                children = record['continuations']
                if not isinstance(children, dict) or len(children) > MAX_ATTEMPTS:
                    raise ValueError()
                for session, child in children.items():
                    if not text(session) or not isinstance(child, dict) or set(child) != {'resource', 'delivery', 'state'} \
                            or not text(child['resource']) or not text(child['delivery']) or child['state'] not in CHILD_STATES:
                        raise ValueError()
            for group, token in value['groups'].items():
                if not text(group, 512) or token not in value['claims']:
                    raise ValueError()
                record = value['claims'][token]
                if group != self.group(record) or record['state'] in ('completed', 'rejected', 'awaiting_owner', 'owner_turn'):
                    raise ValueError()
            if len(json.dumps(value, ensure_ascii=False).encode('utf-8')) > MAX_BYTES:
                raise ValueError()
        except (KeyError, TypeError, ValueError, UnicodeError, VqroError):
            raise VqroError('cutover journal is unsupported; no native action or replay', 'unavailable') from None
        return copy.deepcopy(value), entry['version']

    @staticmethod
    def group(record):
        return record['runtime'] + ':' + record['setup_id']

    def write(self, value, revision):
        if len(value['claims']) > MAX_ATTEMPTS or len(value['groups']) > MAX_ATTEMPTS \
                or len(json.dumps(value, ensure_ascii=False).encode('utf-8')) > MAX_BYTES:
            raise VqroError('cutover journal is full; no automatic eviction/replay', 'overloaded')
        # No retry: an adopted-but-unconfirmed write must not become a native effect.
        return self.ext.vqro.call('operational.apply', {'domain': DOMAIN, 'ops': [
            {'op': 'set', 'key': KEY, 'expect': revision, 'value': value}],
            'note': 'accounts: protected cutover/new-turn intent'})

    def lookup(self, token):
        if not text(token, 32) or len(token) != 32 or any(char not in '0123456789abcdef' for char in token):
            raise VqroError('cutover claim is invalid', 'invalid')
        value, revision = self.read()
        record = value['claims'].get(token)
        if record is None:
            raise VqroError('cutover claim is not acknowledged', 'not_found')
        # All other blocking reads precede the policy used for this allowance.
        # Callbacks consume this projection, never mix it with a later settings read.
        accounts,registry_revision=account_snapshot(self.ext)
        if registry_revision!=record['_account_revision']:
            raise VqroError('account registry changed after claim; no native effect authorized','stale')
        targets=record['authorized_targets'] or [record['account']]
        by_id={account['id']:account for account in accounts}
        if any(target not in by_id or selection_problem(by_id[target])
               or by_id[target]['runtime']!=record['runtime'] or by_id[target]['kind']!='subscription'
               for target in targets):
            raise VqroError('claimed credential target is no longer eligible; no native effect authorized','stale')
        configured=settings(self.ext);now=self.clock()
        if type(now) not in (int,float) or not math.isfinite(now) or now<0:
            raise VqroError('local time unavailable; no native switch authorized','unavailable')
        others={key:old for key,old in value['claims'].items() if key!=token}
        available=configured['auto_cutover_max_per_hour']-max(hourly_counts(others,record['runtime'],record['affected'],now),default=0)
        maximum=min(len(record['authorized_targets']),max(0,available)) if configured['blocked']=='auto_restart' else 0
        blocked_ids={account['id'] for account in accounts if credential_key(account) in value['blocks']
                     and block_active(value['blocks'][credential_key(account)],now)}
        return dict(record, version=revision, max_switches=maximum, known_blocked_targets=sorted(blocked_ids))

    def remaining(self,runtime,affected):
        value,_=self.read();now=self.clock();maximum=settings(self.ext)['auto_cutover_max_per_hour']
        if type(now) not in (int,float) or not math.isfinite(now) or now<0:return 0
        return max(0,maximum-max(hourly_counts(value['claims'],runtime,affected,now),default=0))

    def blocked_accounts(self,accounts):
        value,_=self.read();now=self.clock()
        if type(now) not in (int,float) or not math.isfinite(now) or now<0:return {row['id'] for row in accounts}
        return {row['id'] for row in accounts if credential_key(row) in value['blocks']
                and block_active(value['blocks'][credential_key(row)],now)}

    def known_blocked(self,account):
        return account['id'] in self.blocked_accounts([account])

    def observe_limits(self,observed,accounts):
        value,revision=self.read();now=self.clock();changed=False
        if type(now) not in (int,float) or not math.isfinite(now) or now<0:
            raise VqroError('local time unavailable; refuse AUTO','unavailable')
        by_id={row['id']:row for row in accounts}
        for resource in observed:
            account=by_id.get(configured_id(resource));native=facts(resource)
            if account is None or resource.get('kind')!='agent' or resource.get('available') is not True or native.get('lifecycle')!='live':continue
            availability=native.get('availability');reported=availability.get('native') if isinstance(availability,dict) else None
            reported=reported if isinstance(reported,dict) else {}
            limited=blocked(resource,limit_only=True) or (reported.get('subscription_limit') is True
                    and reported.get('terminal_reason')=='blocking_limit'
                    and reported.get('session_id')==(native.get('session') or {}).get('id'))
            if account['runtime']=='pi':
                from pi_accounts.limits import match_limit
                completion=native.get('completion');completion=completion if isinstance(completion,dict) else {}
                detail=completion.get('native');detail=detail if isinstance(detail,dict) else {}
                error=completion.get('error');error=error if isinstance(error,dict) else {}
                record={'stopReason':detail.get('stopReason'),'timestamp':detail.get('timestamp'),
                        'errorMessage':error.get('message'),'complete':detail.get('error_message_complete') is True}
                limited=limited or match_limit(account.get('provider'),record) is not None
            # A bare percentage is not a verdict. Complete, scoped native exhaustion
            # is negative evidence; an explicit allowed verdict overrides that hint.
            exhausted=capacities([resource],[account]).get(account['id'])==0 and reported.get('ordinaryUsageAllowed') is not True
            if not limited and not exhausted:continue
            marker=limit_marker(resource) if limited else hashlib.sha256(json.dumps(native.get('usage_limits'),sort_keys=True).encode()).hexdigest()
            key=credential_key(account)
            if key is None or value['blocks'].get(key,{}).get('episode')==marker:continue
            resets=[]
            for limit in native.get('usage_limits') or []:
                if isinstance(limit,dict) and (limit.get('used_percent')==100 or limit.get('remaining')==0):
                    reset=limit.get('resets_at_ms')
                    if type(reset) in (int,float) and math.isfinite(reset) and reset/1000>now:resets.append(reset/1000)
            self.mark_blocked(value,account,now,max(resets) if resets else None,marker)
            changed=True
        if changed:self.write(value,revision)

    @staticmethod
    def mark_blocked(value,account,now,reset_at=None,marker='cutover'):
        key=credential_key(account)
        if key is None:return
        if key not in value['blocks'] and len(value['blocks'])>=128:
            raise VqroError('known-blocked journal is full; refuse rather than forget limits','unavailable')
        value['blocks'][key]={'observed_at':now,'reset_at':reset_at,'episode':marker}

    def claim(self, source, target, registry_revision, plan, automatic, message, *, background_loss_disclosed=False, continuation_parent=None):
        native = facts(source)
        identity = {'resource': source.get('id'), 'host': source.get('host'),
                    'session': (native.get('session') or {}).get('id'), 'epoch': native.get('epoch'),
                    'incarnation': source.get('incarnation')}
        if not all(text(item) for item in identity.values()) or type(registry_revision) is not int \
                or registry_revision < 0 or type(automatic) is not bool or not continuation_text(message):
            raise VqroError('cutover source or new-turn configuration is incomplete', 'unavailable')
        if not isinstance(target, dict) or not text(target.get('id'), 64) or target.get('runtime') not in ('pi', 'codex', 'claude'):
            raise VqroError('cutover target is invalid', 'invalid')
        if not isinstance(plan, dict) or not text(plan.get('setup_id')) or not text(plan.get('membership_revision')):
            raise VqroError('cutover setup membership is unavailable', 'unavailable')
        affected = plan.get('affected')
        if not isinstance(affected, list) or not 1 <= len(affected) <= MAX_ATTEMPTS:
            raise VqroError('cutover affected set is incomplete', 'unavailable')
        affected = [member(item) for item in affected]
        if any(item['account'] != target['id'] for item in affected):
            raise VqroError('prepared membership must bind the configured target account', 'stale')
        if len({item['resource'] for item in affected}) != len(affected) or not any(
                all(item[key] == expected for key, expected in identity.items()) for item in affected):
            raise VqroError('cutover source is not in the exact prepared membership', 'stale')
        native_episode = plan.get('native_episode')
        if native_episode is not None and not text(native_episode):
            raise VqroError('native limit episode is invalid', 'invalid')
        inference = plan.get('inference')
        if inference is not None and (not isinstance(inference, dict)
                or set(inference) != {'table_version', 'entry', 'label'}
                or type(inference['table_version']) is not int or inference['table_version'] <= 0
                or not text(inference['entry'], 128) or not text(inference['label'], 512)):
            raise VqroError('unsupported native integration inference metadata', 'invalid')
        if background_loss_disclosed and target['runtime'] != 'claude':
            raise VqroError('background-loss override is not declared by this runtime', 'unsupported')
        authority = self.authorization(automatic, background_loss_disclosed=background_loss_disclosed)
        targets=plan.get('authorized_targets',[target['id']])
        if not isinstance(targets,list) or not 1<=len(targets)<=10 or targets[0]!=target['id'] \
                or not all(text(item,64) for item in targets) or len(set(targets))!=len(targets) \
                or target['runtime']!='codex' and len(targets)!=1:
            raise VqroError('invalid common authorized target order','invalid')
        maximum = settings(self.ext)['auto_cutover_max_per_hour']
        if automatic and maximum == 0 and continuation_parent is None:
            raise VqroError('AUTO hourly budget is zero; offer a human choice instead', 'offer_required')
        now = self.clock()
        if type(now) not in (int, float) or not math.isfinite(now) or now < 0:
            raise VqroError('local time unavailable; offer a human choice instead', 'offer_required')
        value, revision = self.read()
        # Include future timestamps after clock rollback. Count protected intents,
        # including unknown outcomes, rather than pretending only proven effects cost budget.
        if continuation_parent is not None:
            parent=value['claims'].get(continuation_parent)
            expected={**identity,'account':configured_id(source),'native_episode':native_episode,'episode':episode(source)}
            if target['runtime']!='codex' or parent is None or parent['state'] not in ('native_ready','completed','awaiting_owner','owner_turn') \
                    or expected not in parent['member_episodes'] or parent['account']!=target['id'] \
                    or not any(item['session']==identity['session'] and item['account']==target['id'] for item in parent['resumed_members']):
                raise VqroError('continuation-only child lacks original episode/confirmed parent restore','forbidden')
        if automatic and continuation_parent is None and any(count+len(targets) > maximum for count in hourly_counts(
                value['claims'], target['runtime'], affected, now)):
            raise VqroError('AUTO hourly budget exhausted; offer a human choice instead', 'offer_required')
        if automatic and continuation_parent is None and credential_key(target) in value['blocks'] and block_active(value['blocks'][credential_key(target)],now):
            raise VqroError('target is known blocked; never auto ping-pong','offer_required')
        accounts,current_revision=account_snapshot(self.ext)
        if len(targets)>1:
            by_id={account['id']:account for account in accounts}
            source_row=by_id.get(configured_id(source));source_key=credential_key(source_row) if source_row else None
            keys={source_key} if source_key else set()
            if current_revision!=registry_revision:raise VqroError('authorized target registry changed','stale')
            for identifier in targets:
                account=by_id.get(identifier)
                key=credential_key(account) if account is not None else None
                if account is None or account['runtime']!='codex' or account['kind']!='subscription' or selection_problem(account) \
                        or key is None or key in keys or key in value['blocks'] and block_active(value['blocks'][key],now):
                    raise VqroError('authorized target is blocked, ambiguous or aliases an earlier target','offer_required')
                keys.add(key)
        origin=next((account for account in accounts if account['id']==configured_id(source)),None)
        if automatic and origin is not None:
            resets=[]
            for limit in facts(source).get('usage_limits') or []:
                if isinstance(limit,dict) and (limit.get('used_percent')==100 or limit.get('remaining')==0):
                    reset=limit.get('resets_at_ms')
                    if type(reset) in (int,float) and math.isfinite(reset) and reset/1000>now:resets.append(reset/1000)
            self.mark_blocked(value,origin,now,max(resets) if resets else None,limit_marker(source))
        marker = episode(source)
        record = {'state': 'claimed', **{key: identity[key] for key in ('resource', 'host', 'session')},
                  'episode': marker, 'account': target['id'], '_account_revision': registry_revision,
                  'automatic': automatic, 'setup_id': plan['setup_id'], 'membership_revision': plan['membership_revision'],
                  'affected': affected, 'runtime': target['runtime'], 'source_incarnation': identity['incarnation'],
                  'source_epoch': identity['epoch'], 'source_account': configured_id(source), 'native_episode': native_episode,
                  'continuation_text': message, 'continuations': {}, 'authorization': authority, 'created_at': now, 'inference': copy.deepcopy(inference),
                  'continuation_cancelled': False,
                  'source_history': ((facts(source).get('session') or {}).get('native') or {}).get('file') if target['runtime']=='pi' else None,
                  'source_principal': facts(source).get('tool_principal') if target['runtime']=='pi' else None,
                  'authorized_targets':list(targets),'attempted_targets':None,'resumed_members':[],'continuation_owner':None,'member_episodes':copy.deepcopy(plan.get('member_episodes',[])),
                  'continuation_parent':continuation_parent,'resume_launch':None}
        if self.group(record) in value['groups']:
            raise VqroError('another cutover owns this shared setup; never overlap switches', 'busy')
        if any(old['runtime'] == target['runtime'] and (continuation_parent is not None or old['setup_id'] == plan['setup_id'])
               and old['session'] == identity['session'] and old['native_episode'] == native_episode
               and (native_episode is not None or old['episode'] == marker)
               for old in value['claims'].values()):
            raise VqroError('this source episode already has a cutover intent; never replay it', 'conflict')
        token = uuid.uuid4().hex
        value['claims'][token] = record
        value['groups'][self.group(record)] = token
        self.write(value, revision)
        return token

    def validate(self, token, source, native_episode):
        """Recheck the exact source/episode/authority and latest rolling budget."""
        value, revision = self.read()
        record = value['claims'].get(token)
        native = facts(source)
        expected = {'resource': source.get('id'), 'host': source.get('host'),
                    'source_incarnation': source.get('incarnation'), 'source_epoch': native.get('epoch'),
                    'source_account': configured_id(source),
                    'session': (native.get('session') or {}).get('id'), 'episode': episode(source),
                    'native_episode': native_episode, 'state': 'claimed'}
        if record is None or any(record.get(key) != item for key, item in expected.items()):
            raise VqroError('protected source/session/native episode changed', 'stale')
        if self.authorization(record['automatic'], background_loss_disclosed=record['authorization'].get('background_loss_disclosed', False)) != record['authorization']:
            raise VqroError('authenticated manual owner authority changed', 'stale')
        if record['automatic']:
            configured = settings(self.ext)
            now = self.clock()
            if type(now) not in (int, float) or not math.isfinite(now) or now < 0:
                raise VqroError('local time unavailable; offer instead', 'offer_required')
            if configured['blocked'] != 'auto_restart' or any(count > configured['auto_cutover_max_per_hour']
                    for count in hourly_counts(value['claims'], record['runtime'], record['affected'], now)):
                raise VqroError('AUTO policy/hourly budget changed; offer instead', 'offer_required')
        return dict(record, version=revision)

    def reject_before_effect(self, token):
        """Only the owner BEFORE invoking a runtime callback may use this path.

        An exception/unknown response after invoking is NOT proof of no effect:
        that goes through native_result(observed=False) and retains the fence.
        """
        value, revision = self.read()
        record = value['claims'].get(token)
        if record is None or record['state'] != 'claimed':
            raise VqroError('cutover rejection no longer matches its undispatched claim', 'stale')
        # A definite rejection before callback dispatch attempted no target.
        # Settle the reservation, but retain this rejected episode tombstone.
        record['attempted_targets'] = []
        record['state'] = 'rejected'
        if value['groups'].get(self.group(record)) == token:
            del value['groups'][self.group(record)]
        self.write(value, revision)

    def native_result(self, token, *, observed, attempted_targets=None, selected_account=None, inspected_blocked=None):
        value, revision = self.read()
        record = value['claims'].get(token)
        if record is None or record['state'] != 'claimed':
            raise VqroError('cutover native result no longer matches its claim', 'stale')
        # "ready" means eligible under the runtime's reported result. It does not
        # invent same-session confirmation for Claude's explicit unconfirmed case.
        if observed and len(record['authorized_targets'])>1 and attempted_targets is None:
            record['state']='unknown'
            self.write(value,revision)
            raise VqroError('multi-target native result omitted attempted prefix; reservation retained','unavailable')
        if observed:
            attempted=[record['authorized_targets'][0]] if attempted_targets is None else attempted_targets
            if not isinstance(attempted,list) or attempted!=record['authorized_targets'][:len(attempted)]:
                raise VqroError('native result did not report an authorized attempted prefix','unavailable')
            if selected_account is not None and (not attempted or selected_account!=attempted[-1]):
                raise VqroError('native selected account does not match its attempted prefix','unavailable')
            observations=[] if inspected_blocked is None else inspected_blocked
            accounts,_=account_snapshot(self.ext);by_id={account['id']:account for account in accounts}
            if not isinstance(observations,list) or len(observations)>len(attempted):
                raise VqroError('invalid inspected-blocked observations; reservation retained','unavailable')
            seen=set();now=self.clock()
            for observation in observations:
                if not isinstance(observation,dict) or set(observation)!={'account','observed_at_ms','reset_at_ms','source'} \
                        or observation.get('account') not in attempted or observation['account'] in seen \
                        or observation.get('source')!='codex.account/rateLimits':
                    raise VqroError('unbound native blocked observation; reservation retained','unavailable')
                for field in ('observed_at_ms','reset_at_ms'):
                    stamp=observation[field]
                    if field=='reset_at_ms' and stamp is None:continue
                    if type(stamp) is not int or stamp<0:raise VqroError('invalid native observation time','unavailable')
                account=by_id.get(observation['account'])
                if account is None:raise VqroError('observed account disappeared','unavailable')
                reset=observation['reset_at_ms']
                self.mark_blocked(value,account,now,reset/1000 if reset is not None else None,'inspection:'+token+':'+account['id'])
                seen.add(account['id'])
            record['attempted_targets']=list(attempted)
            if selected_account is not None:
                record['account']=selected_account
                for item in record['affected']:item['account']=selected_account
        # Unknown retains the entire reservation even if a partial prefix was reported.
        record['state'] = 'native_ready' if observed else 'unknown'
        self.write(value, revision)

    def consume_resume(self,token):
        """One owner-journal launch intent; never a second/private replay ledger."""
        caller=self.ext.caller() or {};principal=caller.get('principal')
        value,revision=self.read();record=value['claims'].get(token)
        if record is None or record['runtime']!='pi' or record['state']!='claimed' or record['resume_launch'] is not None:
            raise VqroError('Pi resume launch already attempted or not authorized; never replay','conflict')
        if record['automatic']:
            if principal!='extension:accounts':raise VqroError('Pi resume requires the authenticated accounts invocation','forbidden')
        else:
            validate_claim_authority(self.ext,record,False)
            if principal!=record['authorization']['principal']:
                raise VqroError('Pi resume caller does not match the recorded owner invocation','forbidden')
        record['resume_launch']={'state':'claimed','principal':principal}
        self.write(value,revision)
        return {'consumed':True}

    def resume_result(self,token,observed):
        caller=self.ext.caller() or {};value,revision=self.read();record=value['claims'].get(token)
        launch=record.get('resume_launch') if record else None
        if not launch or launch['state']!='claimed' or caller.get('principal')!=launch['principal']:
            raise VqroError('Pi resume result does not match its launch intent','forbidden')
        launch['state']='observed' if observed else 'unknown'
        self.write(value,revision)

    def record_restored(self,token,members):
        value,revision=self.read();record=value['claims'].get(token)
        if record is None or record['state']!='native_ready':raise VqroError('native restore is not confirmed','stale')
        record['resumed_members']=[member(item) for item in members]
        self.write(value,revision)

    def await_owner(self,token,members):
        value,revision=self.read();record=value['claims'].get(token)
        if record is None or record['state']!='native_ready' or record['runtime']!='codex' or record['continuations']:
            raise VqroError('owner offer does not match a restored native result','stale')
        record['resumed_members']=[member(item) for item in members]
        if sorted(item['session'] for item in members)!=sorted(item['session'] for item in record['affected']):
            raise VqroError('owner offer lost native conversations','stale')
        record['state']='awaiting_owner'
        if value['groups'].get(self.group(record))==token:del value['groups'][self.group(record)]
        self.write(value,revision)

    def authorize_owner_continuation(self,token):
        authority=self.authorization(False)
        value,revision=self.read();record=value['claims'].get(token)
        if record is None or record['state']!='awaiting_owner' or record['continuations']:
            raise VqroError('this continuation choice was already consumed; never resend','conflict')
        record['continuation_owner']=authority;record['state']='owner_turn'
        self.write(value,revision)
        return record

    def continuation_allowed(self, token):
        value, _ = self.read()
        record = value['claims'].get(token)
        if record is not None and record['state']=='owner_turn':
            validate_claim_authority(self.ext,{'authorization':record['continuation_owner'],'runtime':record['runtime']},False)
            return True
        if record is None or record['state'] != 'native_ready' or not record['automatic'] or record['continuation_cancelled']:
            return False
        validate_claim_authority(self.ext, record, True)
        configured = settings(self.ext)
        now = self.clock()
        if type(now) not in (int, float) or not math.isfinite(now) or now < 0:
            return False
        return configured['blocked'] == 'auto_restart' and configured['auto_cutover_max_per_hour'] > 0 \
            and all(count <= configured['auto_cutover_max_per_hour'] for count in hourly_counts(
                value['claims'], record['runtime'], record['affected'], now)) \
            and configured['continuation_text'] == record['continuation_text']

    def cancel_continuations(self, token):
        """Only before an unsubmitted child's native call; never clear uncertainty."""
        value, revision = self.read()
        record = value['claims'].get(token)
        if record is None or record['state'] != 'native_ready' or any(
                child['state'] == 'unknown' for child in record['continuations'].values()):
            raise VqroError('uncertain continuation retains its setup fence', 'unavailable')
        for child in record['continuations'].values():
            if child['state'] == 'claimed': child['state'] = 'cancelled'
        record['continuation_cancelled'] = True
        self.write(value, revision)
        self.complete(token)

    def claim_continuation(self, token, affected, *, destination=None):
        if not self.continuation_allowed(token):
            raise VqroError('current policy no longer authorizes an automatic turn', 'policy_changed')
        value, revision = self.read()
        record = value['claims'].get(token)
        if record is None or record['state'] not in ('native_ready','owner_turn') or not record['automatic']:
            raise VqroError('no new turn is authorized by this native result', 'unavailable')
        if not isinstance(affected,dict) or affected.get('session')!=record['session']:
            raise VqroError('each claim authorizes only its original conversation','forbidden')
        if not isinstance(affected, dict) or not text(affected.get('session')) or not text(affected.get('resource')) \
                or not any(item['session'] == affected['session'] and item['resource'] == affected['resource']
                           and item['host'] == affected.get('host') for item in record['affected']):
            raise VqroError('continuation does not name a prepared native conversation', 'stale')
        session = affected['session']
        if session in record['continuations']:
            raise VqroError('new continuation turn was already attempted; never resend it', 'conflict')
        destination = affected if destination is None else destination
        if not isinstance(destination, dict) or not text(destination.get('resource')) or not text(destination.get('host')):
            raise VqroError('continuation destination is incomplete', 'stale')
        delivery = 'd:' + uuid.uuid4().hex
        record['continuations'][session] = {'resource': destination['resource'], 'delivery': delivery, 'state': 'claimed'}
        self.write(value, revision)
        return delivery

    def continuation_result(self, token, affected, *, observed):
        value, revision = self.read()
        record = value['claims'].get(token)
        child = record['continuations'].get(affected.get('session')) if record else None
        if child is None or child['state'] != 'claimed':
            raise VqroError('continuation result no longer matches its intent', 'stale')
        child['state'] = 'observed' if observed else 'unknown'
        self.write(value, revision)

    def complete(self, token):
        value, revision = self.read()
        record = value['claims'].get(token)
        if record is None or record['state'] not in ('native_ready','owner_turn') or any(
                child['state'] not in ('observed', 'cancelled') for child in record['continuations'].values()) or (
                record['automatic'] and not record['continuation_cancelled']
                and set(record['continuations']) != {record['session']}):
            raise VqroError('unconfirmed cutover/continuation retains its setup fence', 'unavailable')
        record['state'] = 'completed'
        if value['groups'].get(self.group(record)) == token:
            del value['groups'][self.group(record)]
        self.write(value, revision)
