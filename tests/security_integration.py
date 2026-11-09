#!/usr/bin/env python3
"""Synthetic loopback security contracts. STOKE_BIN selects the built daemon.
All HOME/config/ledger data are private temporary fixtures; only owned children stop.
Prints a JSON receipt, exits nonzero on any contract failure.
"""
import base64, contextlib, http.server, json, os, pathlib, socket, subprocess, tempfile, threading, time, urllib.request, urllib.error
BIN = os.environ.get('STOKE_BIN', str(pathlib.Path(__file__).resolve().parents[1] / 'target/debug/stoke'))
seen = []
evidence = []
class Mock(http.server.BaseHTTPRequestHandler):
    def log_message(self, *args): pass
    def do_GET(self):
        self.send_response(200); self.end_headers(); self.wfile.write(b'{"data":[]}')
    def do_POST(self):
        body=json.loads(self.rfile.read(int(self.headers.get('Content-Length',0)))); seen.append((self.path,body))
        if self.path == '/deny':
            out={'block': 'fixture denied'}
        elif self.path == '/post':
            out={'response': {'filtered': True}}
        elif self.path == '/fail':
            self.send_response(503); self.end_headers(); return
        else:
            text=json.dumps(body); mode=next((m for m in ['mixed','streammissing','streamcut','outputonly','negative','missing','partial','zero','invalid'] if m in text),'measured')
            if mode=='invalid':
                self.send_response(200); self.end_headers(); self.wfile.write(b'not json'); return
            usage=None if mode in ['missing','streammissing','streamcut'] else ({'input_tokens':3,'prompt_tokens':3} if mode=='partial' else {'input_tokens':0 if mode=='zero' else 3,'output_tokens':0 if mode=='zero' else 2,'prompt_tokens':0 if mode=='zero' else 3,'completion_tokens':0 if mode=='zero' else 2})
            if mode=='mixed' and len(seen)%2==0:
                usage=None; time.sleep(.08)
            if mode=='outputonly': usage={'output_tokens':0,'completion_tokens':0}
            if mode=='negative': usage={'input_tokens':3,'output_tokens':-1,'prompt_tokens':3,'completion_tokens':-1}
            out={'id':'fixture','object':'chat.completion','created':1,'model':'fixture-model','choices':[{'index':0,'message':{'role':'assistant','content':'fixture'}}],'content':[{'type':'text','text':'fixture'}], 'type':'message','role':'assistant','output':[]}
            if usage is not None: out['usage']=usage
            if body.get('stream'):
                self.send_response(200); self.send_header('Content-Type','text/event-stream'); self.end_headers()
                if mode not in ['streammissing','streamcut']:
                    if self.path.endswith('/messages'):
                        wire='data: '+json.dumps({'type':'message_start','message':{'usage':usage}})+'\n\ndata: '+json.dumps({'type':'message_delta','usage':usage})+'\n\n'
                    elif self.path.endswith('/responses'):
                        wire='data: '+json.dumps({'type':'response.completed','response':{'usage':usage}})+'\n\n'
                    else: wire='data: '+json.dumps({'choices':[],'usage':usage})+'\n\n'
                else: wire='data: {"choices":[{"delta":{"content":"fixture"}}]}\n\n'
                self.wfile.write(wire.encode()); self.wfile.flush()
                if mode=='streamcut': time.sleep(1)
                try: self.wfile.write(b'data: [DONE]\n\n')
                except (BrokenPipeError,ConnectionResetError): pass
                return
        self.send_response(200); self.send_header('Content-Type','application/json'); self.end_headers(); self.wfile.write(json.dumps(out).encode())
mock=http.server.ThreadingHTTPServer(('127.0.0.1',0),Mock)
threading.Thread(target=mock.serve_forever,daemon=True).start()
def port():
    with socket.socket() as s: s.bind(('127.0.0.1',0)); return s.getsockname()[1]
@contextlib.contextmanager
def daemon(extra='', cap=1.0, typ='openai', feature_scripts=None, admin='operator-fixture', dev=False, auth_keys=True, prices=(1.0,1.0), provider_tier='cloud'):
    with tempfile.TemporaryDirectory(prefix='stoke-security-',dir=os.environ.get('TMPDIR')) as d:
        p=port(); path=pathlib.Path(d)
        config=f'''default_model="fixture-model"\n[server]\nhost="127.0.0.1"\nport={p}\n[[providers]]\nname="fixture"\ntype="{typ}"\nbase_url="http://127.0.0.1:{mock.server_port}/v1"\ntier="{provider_tier}"\nmodels=["fixture-model"]\n[[keys]]\nkey="tenant-a-fixture"\nbudget_usd={cap}\n[[keys]]\nkey="tenant-b-fixture"\nbudget_usd=2.0\n[pricing.models."fixture-model"]\ninput_per_1m={prices[0]}\noutput_per_1m={prices[1]}\n'''+extra
        (path/'stoke.toml').write_text(config)
        if feature_scripts is not None: (path/'hook.js').write_text(feature_scripts)
        env={k:v for k,v in os.environ.items() if not k.startswith('STOKE_')}; env.update(HOME=d,STOKE_LEDGER_PATH=str(path/'ledger.db'),STOKE_API_KEYS='tenant-a-fixture,tenant-b-fixture' if auth_keys else '')
        if admin is not None: env['STOKE_ADMIN_KEY']=admin
        if dev: env['STOKE_DEV']='1'
        log=open(path/'daemon.log','w+'); proc=subprocess.Popen([BIN],cwd=d,env=env,stdout=log,stderr=log)
        try:
            for _ in range(150):
                if proc.poll() is not None: break
                try: urllib.request.urlopen(f'http://127.0.0.1:{p}/health',timeout=.1); break
                except Exception: time.sleep(.02)
            yield p,proc,log
        finally:
            if proc.poll() is None: proc.terminate()
            try: proc.wait(timeout=5)
            except subprocess.TimeoutExpired: proc.kill(); proc.wait()
            log.close()
def req(p,path,body=None,key='tenant-a-fixture',basic=None):
    headers={'Content-Type':'application/json'}
    if key: headers['x-stoke-key']=key
    if basic: headers['Authorization']='Basic '+base64.b64encode(('operator:'+basic).encode()).decode()
    r=urllib.request.Request(f'http://127.0.0.1:{p}'+path,data=None if body is None else json.dumps(body).encode(),headers=headers)
    try:
        with urllib.request.urlopen(r,timeout=5) as x: return x.status,x.read()
    except urllib.error.HTTPError as x: return x.code,x.read()
def body(path,mode='measured',**extra):
    b={'model':'fixture-model'}
    if path.endswith('responses'): b.update(input=mode,max_output_tokens=100)
    else: b.update(messages=[{'role':'user','content':mode}],max_tokens=100)
    b.update(extra); return b
def stats(p,key='tenant-a-fixture'): return json.loads(req(p,'/v1/budget',key=key)[1])
results=[]
def check(name,fn):
    try: fn(); results.append({'test':name,'ok':True})
    except Exception as e: results.append({'test':name,'ok':False,'error':repr(e)})
def startup(extra):
    with daemon(extra) as (p,proc,log):
        assert proc.poll() is not None,'unsafe config started'; log.seek(0); assert '[[keys]]' in log.read()
check('route_cap_startup',lambda:startup('[[routes]]\nname="guard"\npath="/v1/guard/completions"\nbudget_usd=1.0\n'))
check('route_rate_startup',lambda:startup('[[routes]]\nname="guard"\npath="/v1/guard/completions"\nrate_limit=2\n'))
def scripts_startup():
    with daemon('[plugins]\nscripts=["missing-required-fixture.js"]\n') as (_,proc,_): assert proc.poll() is not None,'configured missing script silently ignored'
check('required_scripts_startup',scripts_startup)
def empty_native_script_startup():
    with daemon('[plugins]\nscripts=["hook.js"]\n',feature_scripts='') as (_,proc,_):
        assert proc.poll() is not None,'empty/no-hook JS script started'
check('empty_native_script_startup',empty_native_script_startup)
def admin():
    with daemon() as (p,_,_):
        for path in ['/ui','/ui/summary','/ui/events','/ui/dashboard.css','/ui/htmx.min.js','/ui/sse.js']:
            assert req(p,path,key=None,basic='tenant-a-fixture')[0]==401,path
        assert req(p,'/ui/summary',key=None,basic='operator-fixture')[0]==200
        assert req(p,'/ui/demo',{},key=None,basic='operator-fixture')[0] in (400,401,403)
        s=stats(p); assert len(s['keys'])==1; assert s.get('receipts') is None,'global receipts exposed'
        assert stats(p,'tenant-b-fixture')['keys'][0]['limit_usd']==2
        assert req(p,'/v1/chat/completions',body('/v1/chat/completions'),key='operator-fixture')[0]==401
        # Generate another tenant's activity: caller receipts stay withheld.
        assert req(p,'/v1/chat/completions',body('/v1/chat/completions','tenant-b-event'),key='tenant-b-fixture')[0]==200
        assert stats(p).get('receipts') is None
        assert stats(p)['keys'][0]['spend_usd']==0
        assert req(p,'/ui/demo',{},key='tenant-b-fixture',basic='operator-fixture')[0]==202
        time.sleep(.2)
        assert stats(p)['keys'][0]['spend_usd']==0
        assert stats(p,'tenant-b-fixture')['keys'][0]['spend_usd']>.000005
        with urllib.request.urlopen(urllib.request.Request(f'http://127.0.0.1:{p}/ui/events',headers={'Authorization':'Basic '+base64.b64encode(b'operator:operator-fixture').decode()}),timeout=2) as response: assert response.status==200
check('admin_tenant_separation',admin)
def admin_empty_and_dev():
    for admin in [None,'','   ']:
        with daemon(admin=admin) as (p,_,_):
            assert req(p,'/ui',key=None,basic='tenant-a-fixture')[0]==401
            assert req(p,'/ui',key=None,basic='operator-fixture')[0]==401
    with daemon(dev=True) as (p,_,_): assert req(p,'/ui',key=None)[0]==401
    with daemon(dev=True,auth_keys=False) as (p,_,_): assert req(p,'/ui',key=None)[0]==200
check('empty_admin_and_explicit_dev_rules',admin_empty_and_dev)
def workloads():
    with daemon(cap=.003) as (p,_,_):
        for i, extra in enumerate([dict(max_tokens=None,max_completion_tokens=10000),dict(n=0),dict(n='2'),dict(max_completion_tokens=-1),dict(n=999),dict(n_samples='bad')]):
            b=body('/v1/chat/completions','bounds-'+str(i),**extra); b={k:v for k,v in b.items() if v is not None}
            before=len(seen); code,_=req(p,'/v1/chat/completions',b); assert code>=400,(extra,code); assert len(seen)==before
        b=body('/v1/chat/completions'); b.pop('max_tokens'); code,_=req(p,'/v1/chat/completions',b); assert code==200,code
        assert seen[-1][1].get('max_tokens') or seen[-1][1].get('max_completion_tokens'),'omitted bound not sent'
        b=body('/v1/chat/completions','valid-alternate',max_completion_tokens=10); b.pop('max_tokens')
        assert req(p,'/v1/chat/completions',b)[0]==200
        assert seen[-1][1]['max_completion_tokens']==10
        assert seen[-1][1].get('max_tokens') is None
check('workload_bounds',workloads)
def unsupported():
    with daemon() as (p,_,_):
        for extra in [dict(messages=[{'role':'user','content':[{'type':'image_url','image_url':{'url':'http://127.0.0.1/image'}}]}]),dict(routing='deliberation'),dict(routing='parallel_vote'),dict(routing='cascade'),dict(routing='self_consistency'),dict(routing='stream_race',stream=True)]:
            before=len(seen); code,_=req(p,'/v1/chat/completions',body('/v1/chat/completions',**extra)); assert code>=400,(extra,code); assert len(seen)==before
check('unsupported_capped_workloads',unsupported)
for path in ['/v1/chat/completions','/v1/messages','/v1/responses']:
    def billing(path=path):
        with daemon(typ='anthropic' if path.endswith('messages') else 'openai') as (p,_,_):
            for mode in ['missing','partial','outputonly','negative','invalid','streammissing','zero','measured']:
                before=stats(p)['keys'][0]['spend_usd']; code,raw= req(p,path,body(path,mode,stream=mode=='streammissing'))
                time.sleep(.04); s=stats(p)['keys'][0]; delta=s['spend_usd']-before
                if mode in ['missing','partial','outputonly','negative','invalid','streammissing']: assert delta>=.0001,(path,mode,code,delta)
                elif mode=='zero': assert abs(delta)<1e-10,(path,mode,delta)
                else: assert abs(delta-.000005)<1e-9,(path,mode,delta)
                assert s['reserved_usd']==0,s
                if path.endswith('completions') and code==200 and mode!='streammissing':
                    parsed=json.loads(raw); assert parsed['stoke_cost_estimated'] is (mode not in ['zero','measured'])
                    assert abs(parsed['stoke_cost']['cost_usd']-delta)<1e-9
                evidence.append({'path':path,'usage_mode':mode,'status':code,'charged_usd':delta,'estimated_usd':s['estimated_usd'],'reserved_usd':s['reserved_usd']})
    check('settlement_'+path,billing)
    def policy(path=path):
        with daemon(f'[plugins]\nprompt_filter=["http://127.0.0.1:{mock.server_port}/deny"]\n',typ='anthropic' if path.endswith('messages') else 'openai') as (p,_,_):
            before=len([x for x in seen if x[0]!='/deny']); code,_=req(p,path,body(path)); assert code>=400,code
            assert len([x for x in seen if x[0]!='/deny'])==before,'provider reached'
    check('policy_'+path,policy)
def stream_usage():
    with daemon() as (p,_,_):
        code,_=req(p,'/v1/chat/completions',body('/v1/chat/completions',stream=True,stream_options={'include_usage':False})); assert code==200
        assert seen[-1][1]['stream_options']['include_usage'] is True
check('forced_stream_usage',stream_usage)
def protected_stream():
    with daemon('[plugins]\npost_response=["http://127.0.0.1:1/mandatory"]\n') as (p,_,_):
        before=len(seen); code,_=req(p,'/v1/chat/completions',body('/v1/chat/completions',stream=True)); assert code>=400; assert len(seen)==before
check('mandatory_output_stream',protected_stream)
def tools_bound():
    with daemon(cap=.003) as (p,_,_):
        before=len(seen)
        b=body('/v1/chat/completions',tools=[{'type':'function','function':{'name':'fixture','description':'x'*6000,'parameters':{'type':'object','properties':{'x':{'type':'string'}}}}}])
        assert req(p,'/v1/chat/completions',b)[0]==429
        assert len(seen)==before
        b['tools'][0]['function']['description']='small fixture'
        assert req(p,'/v1/chat/completions',b)[0]==200
check('serialized_tools_bound',tools_bound)
def multiplicity():
    with daemon(cap=.003) as (p,_,_):
        before=len(seen); assert req(p,'/v1/chat/completions',body('/v1/chat/completions',max_tokens=1000,n=3))[0]==429
        assert len(seen)==before
check('completion_multiplicity_bound',multiplicity)
for hook in ['pre_request','prompt_filter','post_response','scripts']:
    for alias in ['fixture-model','claude-stoke-local--fixture--fixture-model','stoke-local--fixture--fixture-model','claude-stoke-codex--fixture-model','stoke-codex--fixture-model']:
        def native_hooks(hook=hook,alias=alias):
            extra='[plugins]\n'+(hook+'=["hook.js"]\n' if hook=='scripts' else hook+'=["http://127.0.0.1:'+str(mock.server_port)+'/deny"]\n')
            with daemon(extra,feature_scripts='module.exports = { pre_request(input) { return input; } };') as (p,proc,_):
                # Feature-less builds refuse scripts at startup; JS builds must refuse the native request.
                if proc.poll() is not None:
                    assert hook=='scripts' and os.environ.get('STOKE_TEST_JS')!='1'; return
                before=len(seen)
                for stream in [False,True]:
                    code,_=req(p,'/v1/messages',body('/v1/messages',model=alias,stream=stream)); assert code==403,(hook,alias,code)
                if alias=='fixture-model':
                    for stream in [False,True]:
                        assert req(p,'/v1/responses',body('/v1/responses',stream=stream))[0]==403
                assert len(seen)==before
        check('native_hook_'+hook+'_'+alias,native_hooks)
def post_webhook_success():
    with daemon('[plugins]\npost_response=["http://127.0.0.1:'+str(mock.server_port)+'/post"]\n') as (p,_,_):
        code,raw=req(p,'/v1/chat/completions',body('/v1/chat/completions')); assert code==200,code; assert json.loads(raw)['filtered'] is True
check('chat_post_webhook_success',post_webhook_success)
def post_webhook_failure():
    with daemon('[plugins]\npost_response=["http://127.0.0.1:'+str(mock.server_port)+'/fail"]\n') as (p,_,_):
        before=len(seen); code,_=req(p,'/v1/chat/completions',body('/v1/chat/completions')); assert code>=400
        assert len(seen)==before+2,'provider and required post hook must execute'
        assert abs(stats(p)['keys'][0]['spend_usd']-.000005)<1e-9,'post failure forgave/doubled provider spend'
check('chat_post_webhook_failure',post_webhook_failure)
def interrupted_streams():
    for path in ['/v1/chat/completions','/v1/messages','/v1/responses']:
        with daemon(typ='anthropic' if path.endswith('messages') else 'openai') as (p,_,_):
            r=urllib.request.Request(f'http://127.0.0.1:{p}'+path,data=json.dumps(body(path,'streamcut',stream=True)).encode(),headers={'Content-Type':'application/json','x-stoke-key':'tenant-a-fixture'})
            with urllib.request.urlopen(r,timeout=3) as response: response.read(1)
            for _ in range(30):
                s=stats(p)['keys'][0]
                if s['reserved_usd']==0 and s['spend_usd']>=.0001: break
                time.sleep(.05)
            assert s['spend_usd']>=.0001 and s['estimated_usd']>=.0001 and s['reserved_usd']==0,(path,s)
check('client_disconnect_keeps_original_hold',interrupted_streams)
def bad_config_values():
    for extra,cap in [('[limits]\nassumed_max_output_tokens=0\n',1),('',float('nan')),('',-1)]:
        with daemon(extra,cap=cap) as (_,proc,_): assert proc.poll() is not None,'invalid enforcement numeric value started'
check('config_numeric_validation',bad_config_values)
def bad_prices():
    for prices in [(float('nan'),1),(-1,1),(1,float('inf')),(1,-1),(1e308,1e308)]:
        with daemon(prices=prices) as (_,proc,_): assert proc.poll() is not None,('invalid price started',prices)
check('invalid_price_startup',bad_prices)
def route_builtins_scope():
    extra='[[routes]]\nname="protected"\npath="/v1/protected/completions"\nrouting="single"\nbuiltins=["code_formatter"]\n'
    with daemon(extra,typ='anthropic') as (p,_,_):
        before=len(seen)
        assert req(p,'/v1/protected/completions',body('/v1/chat/completions',stream=True))[0]==403
        assert len(seen)==before
        assert req(p,'/v1/messages',body('/v1/messages'))[0]==200
check('route_builtins_stay_scoped',route_builtins_scope)
def unlimited_mixed_usage():
    with daemon('[limits]\nallow_caller_routing=true\n',cap=0) as (p,_,_):
        code,raw=req(p,'/v1/chat/completions',body('/v1/chat/completions','mixed',routing='self_consistency',n_samples=2))
        assert code==200,code
        result=json.loads(raw)
        assert result['stoke_cost_estimated'] is True,'partial fusion usage mislabeled measured'
        assert result['stoke_cost']['cost_usd']>.000005,'unknown billed leg omitted'
check('unlimited_fusion_partial_usage_is_estimated',unlimited_mixed_usage)
def priced_local_stream_guard():
    for path in ['/v1/chat/completions','/v1/messages']:
        with daemon(typ='openai_compatible',provider_tier='local') as (p,_,_):
            assert req(p,path,body(path,'zero',stream=True))[0]==200
            time.sleep(.05); s=stats(p)['keys'][0]
            assert s['spend_usd']==0 and s['reserved_usd']==0,(path,s)
check('priced_local_stream_owns_and_settles_hold',priced_local_stream_guard)
def metadata_and_schema_are_text_data():
    for path in ['/v1/chat/completions', '/v1/responses']:
        with daemon() as (p,_,_):
            schema={'type':'object','properties':{'type':{'type':'string'},'source':{'type':'string'}}}
            tool={'type':'function','function':{'name':'lookup','parameters':schema}} if path.endswith('completions') else {'type':'function','name':'lookup','parameters':schema}
            b=body(path,metadata={'type':'order','source':'fixture-origin'},tools=[tool])
            before=len(seen); code,_=req(p,path,b)
            assert code==200,('ordinary schema/metadata rejected',path,code)
            assert len(seen)==before+1 and seen[-1][1]['metadata']==b['metadata']
check('metadata_and_function_schema_positive_controls',metadata_and_schema_are_text_data)
def assert_no_example_email(value):
    if isinstance(value,str):
        assert '@example.com' not in value, f'leaked sensitive value: {value}'
    elif isinstance(value,dict):
        for child in value.values(): assert_no_example_email(child)
    elif isinstance(value,list):
        for child in value: assert_no_example_email(child)
def pii_redact_protected_chat_shapes():
    extra='[builtins.pii_redact]\nreplacement="[REDACTED]"\n[[routes]]\nname="pii-protected"\npath="/v1/protected/completions"\nrouting="single"\nbuiltins=["pii_redact"]\n'
    with daemon(extra) as (p,_,_):
        standard = [
            {'role':'user','content':[{'type':'text','text':'first@example.com'},{'type':'text','text':'second@example.com'}]},
            {'role':'assistant','content':None,'tool_calls':[
                {'id':'call_1','type':'function','function':{'name':'lookup','arguments':r'{"name":"contact\u0040example.com","items":["third@example.com",{"name":"fourth@example.com"}]}' }},
                {'id':'call_2','type':'function','function':{'name':'lookup_more','arguments':json.dumps({'rows':[['fifth@example.com'],{'name':'sixth@example.com'}]})}},
            ]},
            {'role':'tool','tool_call_id':'call_1','content':'tool-result@example.com'},
        ]
        before=len(seen); code,_=req(p,'/v1/protected/completions',body('/v1/chat/completions',messages=standard))
        assert code==200,code; assert len(seen)==before+1
        captured=seen[-1][1]['messages']
        assert captured[0]['content'][0]['text']=='[REDACTED]'
        assert captured[0]['content'][1]['text']=='[REDACTED]'
        assert captured[1]['tool_calls'][0]['id']=='call_1'
        assert captured[1]['tool_calls'][0]['function']['name']=='lookup'
        assert captured[1]['tool_calls'][1]['function']['name']=='lookup_more'
        for call in captured[1]['tool_calls']:
            json.loads(call['function']['arguments'])
        assert_no_example_email(captured)

        legacy = [
            {'role':'user','content':'legacy request'},
            {'role':'assistant','content':None,'function_call':{
                'name':'legacy_lookup',
                'arguments':r'{"name":"legacy@example.com","nested":[["seventh@example.com"],{"name":"eighth@example.com"}]}'
            }},
        ]
        before=len(seen); code,_=req(p,'/v1/protected/completions',body('/v1/chat/completions',messages=legacy))
        assert code==200,code; assert len(seen)==before+1
        captured=seen[-1][1]['messages']
        assert captured[1]['function_call']['name']=='legacy_lookup'
        json.loads(captured[1]['function_call']['arguments'])
        assert_no_example_email(captured)
check('pii_redact_protected_chat_shapes',pii_redact_protected_chat_shapes)
def pii_redact_protected_tool_arguments():
    extra='[builtins.pii_redact]\nreplacement="[REDACTED]"\n[[routes]]\nname="pii-protected-tools"\npath="/v1/protected/tools"\nrouting="single"\nbuiltins=["pii_redact"]\n'
    with daemon(extra) as (p,_,_):
        messages = [
            {'role':'assistant','content':None,'tool_calls':[
                {'id':'call_args','type':'function','function':{'name':'lookup','arguments':r'{"name":"contact\u0040example.com","items":["nested@example.com",{"name":"business@example.com"}]}'}}
            ]},
            {'role':'tool','tool_call_id':'call_args','content':'ordinary tool result'},
        ]
        before=len(seen); code,_=req(p,'/v1/protected/tools',body('/v1/chat/completions',messages=messages))
        assert code==200,code; assert len(seen)==before+1
        captured=seen[-1][1]['messages']; call=captured[0]['tool_calls'][0]
        assert call['id']=='call_args' and call['function']['name']=='lookup'
        json.loads(call['function']['arguments'])
        assert_no_example_email(captured)

        legacy = [{'role':'assistant','content':None,'function_call':{
            'name':'legacy_lookup',
            'arguments':r'{"name":"legacy@example.com","nested":[["legacy-nested@example.com"],{"name":"legacy-business@example.com"}]}'
        }}]
        before=len(seen); code,_=req(p,'/v1/protected/tools',body('/v1/chat/completions',messages=legacy))
        assert code==200,code; assert len(seen)==before+1
        captured=seen[-1][1]['messages']; function_call=captured[0]['function_call']
        assert function_call['name']=='legacy_lookup'
        json.loads(function_call['arguments'])
        assert_no_example_email(captured)
check('pii_redact_protected_tool_arguments',pii_redact_protected_tool_arguments)
def pii_argument_validation(kind, case, stream):
    extra='[builtins.pii_redact]\nreplacement="[REDACTED]"\n[[routes]]\nname="pii-protected"\npath="/v1/protected/completions"\nrouting="single"\nbuiltins=["pii_redact"]\n'
    depth = 130 if case in ('deep', 'unprotected') else 32
    arguments = r'{"email":"contact\u0040example.com","pad":' + '[' * depth + '0' + ']' * depth + '}'
    assert json.loads(arguments)['email'] == 'contact@example.com'
    if case == 'malformed': arguments = arguments[:-1]
    if case == 'nonstring': arguments = {'email': 'contact@example.com'}
    function = {'name': 'lookup', 'arguments': arguments}
    message = {'role': 'assistant', 'content': None}
    if kind == 'standard':
        message['tool_calls'] = [{'id': 'call_fixture', 'type': 'function', 'function': function}]
    else:
        message['function_call'] = function
    path = '/v1/chat/completions' if case == 'unprotected' else '/v1/protected/completions'
    with daemon(extra) as (p,_,_):
        before = len(seen)
        code,raw = req(p,path,body('/v1/chat/completions',messages=[message],stream=stream))
        if case in ('deep', 'malformed', 'nonstring'):
            assert code == 403, ('mandatory redaction did not refuse', kind, case, stream, code)
            assert len(seen) == before, 'provider contacted before mandatory redaction succeeded'
            assert b'contact' not in raw and b'example.com' not in raw, 'refusal exposed argument data'
            balance = stats(p)['keys'][0]
            assert balance['spend_usd'] == 0 and balance['reserved_usd'] == 0
        else:
            assert code == 200 and len(seen) == before + 1, (kind, case, code)
            captured = seen[-1][1]['messages'][0]
            call = captured['tool_calls'][0]['function'] if kind == 'standard' else captured['function_call']
            assert call['name'] == 'lookup'
            if case == 'unprotected':
                assert call['arguments'] == arguments, 'unprotected route was changed'
            else:
                assert json.loads(call['arguments'])['email'] == '[REDACTED]'
for kind in ('standard', 'legacy'):
    for case in ('deep', 'malformed', 'nonstring', 'shallow', 'unprotected'):
        for stream in (False, True):
            check(f'pii_arguments_{kind}_{case}_{"stream" if stream else "json"}', lambda kind=kind,case=case,stream=stream: pii_argument_validation(kind,case,stream))
if os.environ.get('STOKE_TEST_JS')=='1':
    for hook in ['pre_request','prompt_filter','post_response']:
        def js_throw(hook=hook):
            script='module.exports = { '+hook+'(input) { throw new Error("fixture policy failure"); } };'
            with daemon('[plugins]\nscripts=["hook.js"]\n',feature_scripts=script) as (p,proc,_):
                assert proc.poll() is None,'JS fixture failed startup'; before=len(seen)
                code,_=req(p,'/v1/chat/completions',body('/v1/chat/completions')); assert code==403,code
                assert len(seen)==before+(1 if hook=='post_response' else 0)
                if hook=='post_response': assert abs(stats(p)['keys'][0]['spend_usd']-.000005)<1e-9
        check('js_execution_'+hook,js_throw)
mock.shutdown(); mock.server_close(); print(json.dumps({'binary':BIN,'results':results,'evidence':evidence,'passed':sum(x['ok'] for x in results),'failed':sum(not x['ok'] for x in results)},indent=2)); raise SystemExit(any(not x['ok'] for x in results))
