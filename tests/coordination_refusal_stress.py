"""Explicit full reviewer regression: python3 -m unittest discover -s tests -p coordination_refusal_stress.py.

The regular component suite uses512 refusals; this retains the full73-agent,
2600-refusal scenario separately so quick checks do not silently become stress runs.
Neither counts as a public-core Part B qualification.
"""
import test_coordination_refusal_recovery as regression


class FullRefusalFlood(regression.RefusalRecoveryTests):
    FLOOD = 2600
