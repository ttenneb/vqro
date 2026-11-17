"""Standalone Pi 1.5 runtime pipes; no prim provider or process supervision wiring."""
from .client import DIALOGS, EFFECTS, Limits, PiRpcClient

__all__ = ["DIALOGS", "EFFECTS", "Limits", "PiRpcClient"]
