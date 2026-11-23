"""Replaceable capacity policy: reported hints for NEW work, never live-auth inference or replay."""
import copy
import hashlib
import json
import math
import time
from vqro_ext import VqroError

POLICY='doc/settings/accounts-policy'
CURSORS='doc/accounts/round-robin/'
DEFAULTS={'new_agents':'ask','blocked':'notify','api_key_overflow':False}
MAX_AGE_MS=60_000
MAX_ATTEMPTS=64
DOMAIN='accounts'
ATTEMPTS='operational/accounts/attempts'


def now_ms(): return int(time.time()*1000)


def validate_settings(value):
    if not isinstance(value,dict) or set(value)-set(DEFAULTS):
        raise VqroError('invalid accounts policy fields','invalid')
    result=dict(DEFAULTS,**value)
    if result['new_agents'] not in ('ask','most_remaining_capacity','round_robin') \
            or result['blocked'] not in ('notify','offer_restart','auto_restart') or type(result['api_key_overflow']) is not bool:
        raise VqroError('invalid accounts policy choice','invalid')
    return result


def effective(value):
    try: return validate_settings(value)
    except VqroError: return dict(DEFAULTS)  # unknown/malformed policy never enables autonomous effects


def settings(ext):
    entry=ext.vqro.call('workspace.get',{'key':POLICY})
    return effective(entry['value'] if entry else {})


def resources(ext):
    result=ext.vqro.call('resource.list',{})
    rows=result.get('resources') if isinstance(result,dict) else None
    if not isinstance(rows,list): raise VqroError('account observations are unavailable','unavailable')
    return rows


def facts(row):
    data=row.get('data') if isinstance(row,dict) else None
    native=data.get('agent') if isinstance(data,dict) else None
    return native if isinstance(native,dict) and type(native.get('v')) is int and native['v']==0 else {}


def configured_id(row):
    account=facts(row).get('account')
    return account.get('id') if isinstance(account,dict) and isinstance(account.get('id'),str) else None


def live(row):
    return row.get('available') is True and facts(row).get('lifecycle')=='live'


def blocked(row, limit_only=False):
    native=facts(row);availability=native.get('availability')
    if not live(row) or not isinstance(availability,dict) or availability.get('state')!='blocked': return False
    reason=availability.get('reason')
    caps=native.get('caps');declared=caps.get('agent.availability') if isinstance(caps,dict) else None
    reasons=declared.get('reasons') if isinstance(declared,dict) else None
    if not isinstance(reasons,list) or reason not in reasons: return False
    return reason in ('quota','rate_limited') if limit_only else True


def episode(row):
    native=facts(row)
    values=[row.get('id'),row.get('incarnation'),native.get('epoch')]
    if any(not isinstance(v,str) or not v or len(v.encode())>256 for v in values):
        raise VqroError('blocked source identity is incomplete','unavailable')
    return hashlib.sha256(json.dumps(values,separators=(',',':')).encode()).hexdigest()


def number(value):
    try: return type(value) in (int,float) and math.isfinite(value) and value>=0
    except OverflowError: return False


def capacities(rows, candidates, *, credential_identities=None):
    """Minimum fresh reported percentage headroom, NOT an availability/entitlement guarantee.
    Positive counts without a denominator are incomparable and never ranked as percentages.
    """
    ids={a['id'] for a in candidates};representatives={}
    identities=credential_identities or {}
    for candidate in candidates:
        identity=identities.get(candidate['id'])
        if identity is not None: representatives.setdefault(identity,candidate['id'])
    def canonical(ident):
        identity=identities.get(ident)
        return representatives.get(identity,ident) if identity is not None else ident
    groups={canonical(ident) for ident in ids}
    samples={ident:{} for ident in groups};clock=now_ms();unknown=set()
    for resource in rows:
        native_ident=configured_id(resource);ident=canonical(native_ident)
        if ident not in groups or not live(resource): continue
        native=facts(resource);incomplete=native.get('incomplete',[])
        if not isinstance(incomplete,list) or any(not isinstance(field,str) or field=='usage_limits'
                or field.startswith(('usage_limits.','usage_limits[')) for field in incomplete):
            unknown.add(ident)  # cut constraints are unknown, never a positive partial subset
        limits=native.get('usage_limits')
        if not isinstance(limits,list) or not limits:
            unknown.add(ident)  # a same-account live source with absent/null/empty limits is not complete capacity
        for row in limits if isinstance(limits,list) else []:
            if not isinstance(row,dict): unknown.add(ident);continue
            scope=row.get('scope');observed=row.get('observed_at_ms');source=row.get('source')
            if not isinstance(scope,dict) or scope.get('kind')!='account' or scope.get('account')!=native_ident:
                unknown.add(ident);continue
            if not isinstance(source,str) or not source or len(source)>128 or not number(observed):
                unknown.add(ident);continue  # cannot order/replace an unidentified constraint
            key=json.dumps([source,scope,row.get('window')],sort_keys=True,separators=(',',':'))
            used=row.get('used_percent');remaining=row.get('remaining');reset=row.get('resets_at_ms')
            headroom=max(0,100-used) if number(used) else None
            if number(remaining) and remaining==0: headroom=0
            if observed>clock or clock-observed>MAX_AGE_MS or (reset is not None and (not number(reset) or reset<=clock)):
                headroom=None  # retain the unresolved window instead of inventing spare capacity by dropping it
            if remaining is not None and not number(remaining): headroom=None
            old=samples[ident].get(key)
            if old is None or observed>old[0]: samples[ident][key]=(observed,headroom)
            elif observed==old[0]:
                samples[ident][key]=(observed,None if headroom is None or old[1] is None else min(headroom,old[1]))
    result={}
    blocked_ids={canonical(configured_id(row)) for row in rows if blocked(row)}
    for ident,windows in samples.items():
        values=[sample[1] for sample in windows.values()]
        result[ident]=min(values) if values and None not in values and ident not in blocked_ids|unknown else None
    return {ident:result[canonical(ident)] for ident in ids}


def pick_new(ext, candidates, runtime, *, credential_identities=None):
    configured=settings(ext);mode=configured['new_agents']
    if mode=='ask': return None
    try: observed=resources(ext)
    except VqroError: return None  # no cached/guessed capacity fallback
    blocked_ids={configured_id(row) for row in observed if blocked(row)}
    eligible=[row for row in candidates if row['id'] not in blocked_ids
              and (row['kind']!='api_key' or configured['api_key_overflow'])]
    if credential_identities is not None:
        blocked_credentials={credential_identities[ident] for ident in blocked_ids
                             if credential_identities.get(ident) is not None}
        distinct=[];seen=set()
        for row in eligible:
            identity=credential_identities.get(row['id'])
            if identity is None or identity in blocked_credentials or identity in seen: continue
            seen.add(identity);distinct.append(row)
        eligible=distinct
    if mode=='most_remaining_capacity':
        scores=capacities(observed,eligible,credential_identities=credential_identities)
        eligible=[row for row in eligible if scores.get(row['id']) is not None and scores[row['id']]>0]
        return max(eligible,key=lambda row:scores[row['id']]) if eligible else None
    if not eligible: return None
    key=CURSORS+runtime
    entry=ext.vqro.call('workspace.get',{'key':key});value=entry['value'] if entry else {}
    last=value.get('last') if isinstance(value,dict) else None
    ids=[row['id'] for row in eligible]
    selected=eligible[(ids.index(last)+1)%len(ids)] if last in ids else eligible[0]
    # A failed/unknown spawn consumes this attempted choice; never retry an effect to fix the cursor.
    ext.apply([{'op':'set','key':key,'expect':entry['version'] if entry else 0,'value':{'last':selected['id']}}],
              'accounts: next new-agent account')
    return selected


class Effects:
    """One small public operational domain, no undo/private file/workspace fallback.
    A claim must be durably acknowledged before a launcher command is invoked once.
    """
    def __init__(self,ext): self.ext=ext;self.registered=False
    def read(self):
        if not self.registered:
            self.ext.vqro.call('operational.register',{'name':DOMAIN,'bytes':128*1024})
            self.registered=True
        reply=self.ext.vqro.call('operational.get',{'domain':DOMAIN,'key':ATTEMPTS})
        entry=reply.get('entry') if isinstance(reply,dict) else None
        if entry is None: return {'attempts':{},'children':[],'hosts':[],'uncertain':{}},0
        value=entry.get('value')
        if not isinstance(value,dict) or set(value)!={'attempts','children','hosts','uncertain'} \
                or not isinstance(value['attempts'],dict) or not isinstance(value['uncertain'],dict) \
                or not isinstance(value['children'],list) or not isinstance(value['hosts'],list) \
                or any(len(value[key])>MAX_ATTEMPTS for key in value):
            raise VqroError('account restart journal is unsupported or full; no automatic action','unavailable')
        text=lambda item,limit:isinstance(item,str) and 0<len(item.encode())<=limit
        token=lambda item:text(item,64) and len(item)==64 and all(c in '0123456789abcdef' for c in item)
        valid=type(entry.get('version')) is int and entry['version']>0
        for marker,attempt in value['attempts'].items():
            valid=valid and token(marker) and isinstance(attempt,dict) \
                and set(attempt)=={'source','target','state'} and text(attempt.get('source'),256) \
                and text(attempt.get('target'),64) and attempt.get('state') in ('claimed','observed','unknown')
        valid=valid and all(text(item,128) for key in ('children','hosts') for item in value[key]) \
            and all(text(ident,64) and token(marker) for ident,marker in value['uncertain'].items())
        if not valid: raise VqroError('account restart journal format is unsupported; no automatic action','unavailable')
        return copy.deepcopy(value),entry['version']
    def write(self,value,revision):
        if any(len(value[key])>MAX_ATTEMPTS for key in value) or len(json.dumps(value).encode())>48*1024:
            raise VqroError('account restart journal is full; launch manually','overloaded')
        return self.ext.vqro.call('operational.apply',{'domain':DOMAIN,'ops':[
            {'op':'set','key':ATTEMPTS,'expect':revision,'value':value}], 'note':'accounts: fresh launch claim/result'})
    @staticmethod
    def suppressed(value,source):
        return source['id'] in value['children'] or source.get('host') in value['hosts'] \
            or configured_id(source) in value['uncertain']
    def claim(self,source,token,target):
        value,revision=self.read()
        if token in value['attempts']:
            raise VqroError('a fresh restart was already attempted; never replay it','conflict')
        if target['id'] in value['uncertain']:
            raise VqroError('another fresh launch on this target has an uncertain outcome','unavailable')
        value['attempts'][token]={'source':source['id'],'target':target['id'],'state':'claimed'}
        value['uncertain'][target['id']]=token
        self.write(value,revision)
    def finish(self,source,token,target,result):
        value,revision=self.read();attempt=value['attempts'].get(token)
        if not attempt or attempt.get('source')!=source['id']: return
        agent=result.get('agent') if isinstance(result,dict) else None
        host=result.get('host') if isinstance(result,dict) else None
        if not host and isinstance(result,dict) and result.get('resource'): host=result['resource']
        valid=lambda item:isinstance(item,str) and 0<len(item.encode())<=128
        observed=isinstance(result,dict) and result.get('outcome')=='observed' and (valid(agent) or valid(host))
        attempt['state']='observed' if observed else 'unknown'
        if observed:
            if valid(agent) and agent not in value['children']: value['children'].append(agent)
            if valid(host) and host not in value['hosts']: value['hosts'].append(host)
            if value['uncertain'].get(target['id'])==token: del value['uncertain'][target['id']]
        self.write(value,revision)
