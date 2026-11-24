"""Late read-expiry and worker lifetime controls, fake only."""
from pathlib import Path
import sys
import unittest
from unittest.mock import patch
sys.path[:0]=[str(Path(__file__).resolve().parents[1]/'extensions'),str(Path(__file__).resolve().parent)]
import test_account_capacity_admission as admission
from test_account_capacity import resource, NOW
import accounts_policy as policy

class StopWorker(BaseException):
    pass

class LateCapacityValidation(unittest.TestCase):
    def test_expiry_during_final_blocking_reads_becomes_unknown_not_positive_or_blocked(self):
        for read in ('runtime','policy'):
            for deadline in ('age','reset'):
                with self.subTest(read=read,deadline=deadline),patch('accounts_policy.now_ms',return_value=NOW) as clock:
                    core,accounts,a,b,_=admission.CapacityAdmission().fixture()
                    limits=core.resources['target']['data']['agent']['usage_limits'][0]
                    if deadline=='age':limits['observed_at_ms']=NOW-policy.MAX_AGE_MS+1
                    else:limits['resets_at_ms']=NOW+1
                    call=core.call;after_runtime=False;advanced=False
                    def delay_validation(method,params):
                        nonlocal after_runtime,advanced
                        result=call(method,params)
                        claimed=core.op_entry and core.op_entry['value'].get('claims')
                        if claimed and method=='workspace.get':
                            if params.get('key')=='doc/settings/pi':
                                after_runtime=True
                                if read=='runtime':clock.return_value=NOW+2;advanced=True
                            elif params.get('key')==policy.POLICY and after_runtime and read=='policy':
                                clock.return_value=NOW+2;advanced=True
                        return result
                    with patch.object(core,'call',side_effect=delay_validation):accounts.handle_blocked('old')
                    self.assertTrue(advanced)
                    self.assertIsNone(policy.capacities([core.resources['target']],[b])['b'])
                    self.assertEqual(len(core.resumes),1);self.assertEqual(len(core.sends),1)
                    self.assertEqual(len(core.op_entry['value']['claims']),1)
                    self.assertEqual(core.launches,[])
        with patch('accounts_policy.now_ms',return_value=NOW):
            core,accounts,a,b,_=admission.CapacityAdmission().fixture()
            core.resources['target']['data']['agent']['usage_limits'][0]['observed_at_ms']=NOW-policy.MAX_AGE_MS+1
            accounts.handle_blocked('old');self.assertEqual(len(core.resumes),1)

    def test_policy_and_hourly_zero_revoked_during_final_read_prevent_native_effects(self):
        for change in ('notify','zero_budget'):
            with self.subTest(change=change),patch('accounts_policy.now_ms',return_value=NOW):
                core,accounts,a,b,configured=admission.CapacityAdmission().fixture()
                call=core.call;after_runtime=False;revoked=False
                def revoke(method,params):
                    nonlocal after_runtime,revoked
                    claimed=core.op_entry and core.op_entry['value'].get('claims')
                    if claimed and method=='workspace.get':
                        if params.get('key')=='doc/settings/pi':after_runtime=True
                        elif params.get('key')==policy.POLICY and after_runtime and not revoked:
                            revoked=True
                            changed={'blocked':'notify'} if change=='notify' else {'auto_cutover_max_per_hour':0}
                            core.apply([{'op':'set','key':policy.POLICY,'value':dict(configured,**changed)}],'late policy revocation')
                    return call(method,params)
                with patch.object(core,'call',side_effect=revoke):accounts.handle_blocked('old')
                self.assertTrue(revoked);self.assertEqual(len(core.op_entry['value']['claims']),1)
                self.assertEqual(core.stops,[]);self.assertEqual(core.resumes,[]);self.assertEqual(core.sends,[])

    def test_overflow_notify_and_logging_failures_do_not_kill_or_strand_worker(self):
        core,accounts,a,b,_=admission.CapacityAdmission().fixture()
        core.resources={str(i):resource(str(i),a,blocked=True) for i in range(65)}
        accounts.changed('missed',{})
        self.assertTrue(accounts.overloaded);self.assertEqual(len(accounts.pending),64)
        handled=[];notify_calls=[]
        class ControlledWake:
            calls=0
            def wait(self):
                self.calls+=1
                if self.calls==2:
                    with accounts.pending_lock:accounts.pending.add('later')
                if self.calls==3:raise StopWorker()
            def clear(self):pass
        class BrokenLog:
            def write(self,text):raise OSError('fake closed log')
            def flush(self):raise OSError('fake closed log')
        def failed_notify(*args,**kwargs):
            notify_calls.append(True);raise TimeoutError('fake ui transport timeout')
        accounts.wake=ControlledWake()
        with patch.object(core,'notify',side_effect=failed_notify),patch.object(accounts,'handle_blocked',side_effect=handled.append),patch('sys.stderr',BrokenLog()):
            with self.assertRaises(StopWorker):accounts.worker()
        self.assertEqual(len(handled),65);self.assertEqual(handled[-1],'later')
        self.assertEqual(len(set(handled)),65);self.assertEqual(notify_calls,[True])

if __name__=='__main__':unittest.main()
