"""Stdlib cross-language canonical fixtures; NOT production API parser qualification."""
import hashlib
import json
import math
from pathlib import Path
import subprocess
import unittest


def pairs(items):
    result = {}
    for key, value in items:
        if key in result:
            raise ValueError('duplicate JSON key')
        result[key] = value
    return result


def parse(raw):
    def integer(token):
        if token == '-0':
            raise ValueError('negative zero')
        return int(token)
    def floating(token):
        value = float(token)
        if not math.isfinite(value) or value != int(value) or abs(value) > 9007199254740991 or (value == 0 and token.startswith('-')):
            raise ValueError('integer JSON only')
        return int(value)
    def invalid(token):
        raise ValueError('non-JSON number')
    return json.loads(raw, object_pairs_hook=pairs, parse_int=integer, parse_float=floating, parse_constant=invalid)


def canonical(value):
    if value is None or isinstance(value, bool):
        return json.dumps(value)
    if isinstance(value, int):
        if abs(value) > 9007199254740991:
            raise ValueError('unsafe integer')
        return str(value)
    if isinstance(value, str):
        value.encode('utf-8', 'strict')
        return json.dumps(value, ensure_ascii=False)
    if isinstance(value, list):
        return '[' + ','.join(canonical(item) for item in value) + ']'
    if isinstance(value, dict):
        keys = sorted(value, key=lambda key: key.encode('utf-8', 'strict'))
        return '{' + ','.join(canonical(key) + ':' + canonical(value[key]) for key in keys) + '}'
    raise ValueError('unsupported value')


class CanonicalContract(unittest.TestCase):
    def test_cross_language_unicode_key_order_and_escapes(self):
        module = (Path(__file__).parent / 'canonical.mjs').resolve().as_uri()
        values = [{"b": 2, "a": 1}, {"\ue000": "bmp", "\U00010000": "astral", "é": "unicode"},
                  {"text": "quote\" slash/ back\\ newline\n tab\t \u2028 😀"}, [None, True, False, 9007199254740991, -1]]
        code = f"import fs from 'node:fs';import {{canonical}} from {json.dumps(module)};console.log(JSON.stringify(JSON.parse(fs.readFileSync(0,'utf8')).map(canonical)));"
        result = subprocess.run(['node', '--input-type=module', '-e', code], input=json.dumps(values), text=True, capture_output=True, check=True)
        self.assertEqual(json.loads(result.stdout), [canonical(value) for value in values])

    def test_duplicate_key_including_escaped_key_rejected(self):
        for raw in ['{"a":1,"a":2}', '{"a":1,"\\u0061":2}', '{"outer":{"a":1,"a":2}}']:
            with self.assertRaises(ValueError):
                parse(raw)

    def test_equivalent_order_and_integer_lexical_forms(self):
        self.assertEqual(canonical(parse('{"b":2,"a":1.0}')), canonical(parse('{"a":1,"b":2}')))
        self.assertEqual(canonical(parse('1e0')), '1')

    def test_invalid_numbers_unicode_and_same_revision_body_change(self):
        for raw in ['NaN', 'Infinity', '1.5', '-0', '-0.0', '9007199254740992', '"\\ud800"']:
            with self.assertRaises((ValueError, UnicodeError)):
                canonical(parse(raw))
        one = hashlib.sha256(canonical({"revision": "1", "body": "one"}).encode()).hexdigest()
        two = hashlib.sha256(canonical({"revision": "1", "body": "two"}).encode()).hexdigest()
        self.assertNotEqual(one, two)


if __name__ == '__main__':
    unittest.main()
