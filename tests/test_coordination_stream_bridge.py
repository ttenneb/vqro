"""Actual SDK notification routing boundary, not direct handler injection."""
import base64
from collections import deque
import json
import threading
import unittest
import test_coordination_engine as fixtures
import vqro_ext as sdk
from coordination import CoordinationService
from coordination_streams import ItemReceiptStreams


class SdkReceiptBridgeTests(unittest.TestCase):
    def setUp(self):
        self.f = fixtures.EngineTests(); self.f.setUp()
        self.f.resources['r1'].update(provider='fake', instance='host1')
        self.f.resources['r1']['data']['agent']['caps']['agent.send']['correlation'] = 'insertion'
        self.delivery = self.f.send()['delivery']
        self.f.resume(); self.f.engine.dispatch_one()
        self.assertTrue(self.f.engine.get(self.delivery)['incomplete'])
        self.client = object.__new__(sdk.Vqro)
        self.client.streams, self.client.early, self.client.evicted = {}, {}, set()
        self.client.early_bytes = 0
        self.client.closed, self.client.closed_lock = deque(maxlen=256), threading.Lock()
        self.calls = []
        ext = object.__new__(sdk.Extension); ext.vqro = self.client
        self.service = object.__new__(CoordinationService); self.service.ext = ext
        self.service.items = self.viewer = ItemReceiptStreams(self.service.rpc, self.f.engine.observe_receipts)
        def call(method, params, *, on_result=None):
            self.calls.append((method, params))
            if method == 'resource.attach':
                frame = b''.join((json.dumps(r) + '\n').encode() for r in (
                    {'op': 'snapshot', 'phase': 'begin', 'epoch': 'receipts', 'coverage': 'tail'},
                    {'op': 'put', 'kind': 'receipt', 'id': 'native', 'rev': 1,
                     'value': {'delivery': self.delivery, 'stage': 'inserted'}},
                    {'op': 'snapshot', 'phase': 'end', 'seq': 0}))
                # This uses the real SDK demultiplexer. Without a registered sink
                # the bytes stay in its early-note buffer, never the handler.
                self.note('stream.data', {'stream': 17, 'data': base64.b64encode(frame).decode()})
                reply = {'outcome': 'accepted', 'stream': 17, 'epoch': 'receipts', 'initial': 'reset'}
                if on_result: on_result(reply)
                return reply
            return {}
        self.client.call = call
        self.viewer.sync(self.f.resources, {'fake': {'generation': 1, 'capabilities': {'items.stream': {}}}})

    def note(self, method, params):
        message = {'jsonrpc': '2.0', 'method': method, 'params': params}
        self.assertTrue(self.client._line(message, len(json.dumps(message))))

    def test_sdk_sink_replays_early_bytes_and_updates_compacted_delivery(self):
        self.viewer.pump(); self.viewer.pump()
        record = self.f.engine.get(self.delivery)
        self.assertEqual(record['state'], 'inserted')
        self.assertEqual(record['result']['receipt']['stage'], 'inserted')
        self.assertIn(17, self.client.streams)
        self.assertEqual(self.client.early, {})
        self.assertEqual(len(self.f.ledger), 1)
        self.assertTrue(all(m in ('resource.attach', 'stream.close') for m, _ in self.calls))

    def test_late_sdk_receipt_updates_evicted_settled_summary_without_replay(self):
        for _ in range(257):
            self.f.send(send_new=True)
            self.assertTrue(self.f.engine.dispatch_one())
        self.assertIsNone(self.f.store.get('message/' + self.delivery))
        count = len(self.f.ledger)
        self.viewer.pump(); self.viewer.pump()
        marker = self.f.engine.get(self.delivery)
        self.assertEqual(marker['state'], 'inserted')
        self.assertTrue(marker['incomplete'])
        self.assertEqual(len(self.f.ledger), count)

    def test_closing_viewer_unregisters_its_sdk_sink_without_touching_others(self):
        self.viewer.pump(); self.viewer.pump()
        other = object(); self.client.streams[99] = other
        self.viewer._close('r1', reset=True, reason='owned test close')
        self.assertNotIn(17, self.client.streams)
        self.assertIs(self.client.streams[99], other)
        self.assertIn(17, self.client.closed)
        self.assertTrue(any(m == 'stream.close' and p['stream'] == 17 for m, p in self.calls))
        self.assertEqual(len(self.f.ledger), 1)
