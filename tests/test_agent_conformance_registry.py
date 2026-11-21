"""Late row imports must bind the current implementations, not stale or overwritten handlers."""
import unittest

from agent_conformance import cases, content_origin_cases, view_causality_cases


class CurrentHandlerRegistryTests(unittest.TestCase):
    def test_c12_uses_current_dispatch_metadata_handler(self):
        self.assertIs(cases.HANDLERS['C12'], view_causality_cases.generic_causality_wire)

    def test_d17_uses_current_model_assertion_handler(self):
        self.assertIs(cases.HANDLERS['D17'], content_origin_cases.core_bound_origin)


if __name__ == '__main__':
    unittest.main()
