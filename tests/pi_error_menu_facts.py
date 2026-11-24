"""Actual Rust Agent facts -> production generic pane flags/menu declaration.

Invoked by the Rust adapter test with synthetic native DTO output, never a runtime.
"""
import json
from pathlib import Path
import sys
sys.path.insert(0,str(Path(__file__).resolve().parents[1]/'extensions'))
from agent_fixture import FakeCore
from agents import Agents
from accounts import Accounts

payload=json.load(sys.stdin)
row=payload['facts']
# These are the core's generic resource envelope, NOT inferred app identity.
row.update(id='synthetic-pi',provider='fixture-provider',available=row['state']=='live')
core=FakeCore('accounts');core.providers['fixture-provider']={'capabilities':payload['capabilities']}
Accounts(core)
agents=object.__new__(Agents);agents.ext=core
flags=agents.pane_menu_flags(row)
context={'resource_kind':row['kind'],**{'menu.'+key:value for key,value in flags.items()}}
print(json.dumps({'context':context,'menu':core.value('menu/accounts.pi-error')}))
