"""Local transport fixture exercising the REAL public SDK error-raising path.

No socket/client constructor, native provider, credential or external effect.
"""
import copy
import threading
from vqro_ext import Vqro


def provider_error(data,method='resource.invoke',params=None,message='synthetic provider refusal'):
    client=object.__new__(Vqro)
    client.lock=threading.Lock();client.waiting={};client.next=1
    def reply(request):
        slot=client.waiting[request['id']]
        slot[1]={'jsonrpc':'2.0','id':request['id'],
                 'error':{'code':-32000,'message':message,'data':copy.deepcopy(data)}}
        slot[0].set()
    client._send=reply
    return client.call(method,params or {})
