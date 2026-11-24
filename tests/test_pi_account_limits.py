"""Human-approved Pi-only exact-message inference; synthetic provider messages."""
import json
from pathlib import Path
import sys
import unittest
sys.path[:0] = [str(Path(__file__).resolve().parents[1] / 'extensions')]
from pi_accounts.limits import match_limit, TABLE


def error(message):
    return {'stopReason': 'error', 'timestamp': 123456, 'errorMessage': message, 'complete': True}


def anthropic(kind='rate_limit_error', message='Request rate limit reached'):
    return '429 ' + json.dumps({'type': 'error', 'error': {'type': kind, 'message': message}, 'request_id': 'req_fake'}, separators=(',', ':'))


class PiLimitMessages(unittest.TestCase):
    def test_every_versioned_entry_has_a_source_and_matches_its_examples(self):
        self.assertEqual(TABLE['version'], 1)
        for row in TABLE['entries']:
            self.assertTrue(row['sources'])
            self.assertTrue(row['examples'])
            for example in row['examples']:
                with self.subTest(entry=row['id'], message=example):
                    matched = match_limit(row['provider'], error(example))
                    self.assertEqual(matched['entry'], row['id'])
                    self.assertEqual(matched['table_version'], 1)
                    self.assertEqual(matched['label'], 'switched because the error looked like a usage limit')

    def test_codex_exact_renderer_near_misses_do_not_match(self):
        for message in ('You have hit your ChatGPT usage limit', 'You have hit your ChatGPT usage limits.',
                        'You have hit your ChatGPT usage limit. Extra instructions',
                        'prefix You have hit your ChatGPT usage limit.',
                        'You have hit your ChatGPT usage limit.\n', 'quota exceeded', '429 rate limit',
                        'You have hit your ChatGPT usage limit. Try again in ~many min.'):
            with self.subTest(message=message): self.assertIsNone(match_limit('openai-codex', error(message)))

    def test_anthropic_matches_only_the_exact_sdk_http429_provider_error_shape(self):
        self.assertIsNotNone(match_limit('anthropic', error(anthropic())))
        for message in (anthropic('overloaded_error'), anthropic().replace('429 ', '500 ', 1),
                        'prefix ' + anthropic(), anthropic() + ' trailing',
                        '429 {"type":"error","error":{"type":"rate_limit_error","message":"ok"},"error":{}}',
                        '429 {"type":"error","error":{"type":"rate_limit_error","message":"ok","unexpected":true}}',
                        '429 rate_limit_error', 'rate_limit_error', 'usage limit reached'):
            with self.subTest(message=message): self.assertIsNone(match_limit('anthropic', error(message)))

    def test_success_content_wrong_provider_missing_record_or_truncation_never_match(self):
        message = TABLE['entries'][0]['examples'][0]
        for record in (dict(error(message), stopReason='stop'), dict(error(message), timestamp=None),
                       dict(error(message), timestamp=True), dict(error(message), complete=False),
                       {'errorMessage': message}, error(message + 'x' * 5000)):
            with self.subTest(record=record): self.assertIsNone(match_limit('openai-codex', record))
        self.assertIsNone(match_limit('anthropic', error(message)))
        self.assertIsNone(match_limit('openai', error(message)))


if __name__ == '__main__': unittest.main()
