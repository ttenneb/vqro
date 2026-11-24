"""Human-approved, Pi-only error-message inference (never a native quota fact).

The explicit exception is intentional: native Pi loses provider limit types.
Match only this one versioned table against a COMPLETE native error record.
No generic quota/rate substring classification; unknowns require an owner click.
"""
import json
from pathlib import Path
import re

TABLE = json.loads(Path(__file__).with_name('limit_messages.json').read_text(encoding='utf-8'))
LABEL = 'switched because the error looked like a usage limit'


def unique_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError('duplicate native error member')
        result[key] = value
    return result


def anthropic_sdk429(message):
    # Exact native SDK serialization, not a scan for English words. The allowed
    # provider error type is documented; unrecognized shapes fail to OFFER.
    if not message.startswith('429 {') or not message.endswith('}'):
        return False
    try:
        body = json.loads(message[4:], object_pairs_hook=unique_object)
    except (ValueError, TypeError):
        return False
    if not isinstance(body, dict) or set(body) - {'type', 'error', 'request_id'} or body.get('type') != 'error':
        return False
    error = body.get('error')
    if not isinstance(error, dict) or set(error) != {'type', 'message'} or error['type'] != 'rate_limit_error':
        return False
    detail = error['message']
    if not isinstance(detail, str) or not detail or len(detail) > 2048 or not detail.isprintable():
        return False
    if 'request_id' in body and (not isinstance(body['request_id'], str) or not 0 < len(body['request_id']) <= 128
                                 or not body['request_id'].isprintable()):
        return False
    return True


def match_limit(provider, record):
    if not isinstance(record, dict) or record.get('stopReason') != 'error' or record.get('complete') is not True \
            or type(record.get('timestamp')) is not int or not 0 <= record['timestamp'] <= 9007199254740991:
        return None
    message = record.get('errorMessage')
    if not isinstance(message, str) or not message or not message.isprintable():
        return None
    try:
        if len(message.encode('utf-8')) > 4096:
            return None
    except UnicodeError:
        return None
    for entry in TABLE['entries']:
        if entry['provider'] != provider:
            continue
        matched = re.fullmatch(entry['pattern'], message) is not None if entry['matcher'] == 'fullmatch' \
            else anthropic_sdk429(message) if entry['matcher'] == 'anthropic_sdk429_json' else False
        if matched:
            return {'table_version': TABLE['version'], 'entry': entry['id'], 'label': LABEL}
    return None
