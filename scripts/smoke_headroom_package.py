#!/usr/bin/env python3
"""Real optional worker + launcher + Stoke; deterministic provider, no inference."""
import argparse
import copy
import json
import os
from pathlib import Path
import socket
import subprocess
import sys
import tempfile

from smoke_headroom import GATEWAY_KEY, SpyState, call, config, start_spy, wait_health


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--binary', required=True, type=Path)
    parser.add_argument('--worker-python', required=True, type=Path)
    parser.add_argument('--integration-dir', type=Path, default=Path(__file__).resolve().parents[1] / 'integrations/headroom')
    args = parser.parse_args()
    state = SpyState()
    provider = start_spy(state, 'provider')
    results = []
    source = json.dumps({'rows': [{'id': n, 'note': 'keep  spaces', 'value': n * 3} for n in range(24)]}, indent=2)
    try:
        with tempfile.TemporaryDirectory(prefix='stoke-package-smoke-') as directory:
            root = Path(directory)
            for enabled in (False, True):
                with socket.socket() as sock:
                    sock.bind(('127.0.0.1', 0))
                    port = sock.getsockname()[1]
                original = config(port, provider.server_port, 1).encode()
                path = root / ('on.toml' if enabled else 'off.toml')
                path.write_bytes(original)
                env = {k: v for k, v in os.environ.items() if not k.startswith(('HEADROOM_', 'STOKE_'))}
                env.update(HOME=str(root), STOKE_API_KEYS=GATEWAY_KEY, STOKE_LEDGER_PATH=str(root / ('on.db' if enabled else 'off.db')))
                command = [sys.executable, str(args.integration_dir / 'launch.py'), '--stoke-bin', str(args.binary.resolve()), '--config', str(path), '--worker-python', str(args.worker_python.absolute())]
                if not enabled:
                    command.append('--off')
                with (root / ('on.log' if enabled else 'off.log')).open('w') as log:
                    proc = subprocess.Popen(command, env=env, stdout=log, stderr=subprocess.STDOUT)
                    try:
                        base = f'http://127.0.0.1:{port}'
                        wait_health(base, proc)
                        for stream in (False, True):
                            body = {'model': 'spy-model', 'reasoning': {'effort': 'high'}, 'tools': [], 'stream': stream,
                                    'input': [{'type': 'function_call_output', 'call_id': 'opaque-call', 'output': source},
                                              {'type': 'reasoning', 'encrypted_content': 'opaque-test'}]}
                            status, headers, raw = call(base, body)
                            assert status == 200, (status, raw)
                            expected = 'compressed' if enabled else 'disabled'
                            assert headers.get('x-stoke-headroom') == expected, headers
                            sent = json.loads(state.provider[-1]['body'])
                            output = sent['input'][0]['output']
                            assert json.loads(output) == json.loads(source)
                            if enabled:
                                assert len(output.encode()) < len(source.encode())
                            else:
                                assert output == source
                            expected_body = copy.deepcopy(body)
                            expected_body['input'][0]['output'] = output
                            assert sent == expected_body, 'non-output request fields changed'
                            if stream:
                                assert raw == b'data: {"type":"response.completed"}\n\n'
                            results.append({'enabled': enabled, 'stream': stream, 'status': expected, 'input_bytes': len(source.encode()), 'output_bytes': len(output.encode())})
                    except Exception as error:
                        log.flush()
                        raise RuntimeError(log.name + '\n' + Path(log.name).read_text()) from error
                    finally:
                        proc.terminate()
                        proc.wait(timeout=15)
                assert path.read_bytes() == original
                assert not list(root.glob('.stoke-headroom-*.toml'))
                with socket.socket() as sock:
                    assert sock.connect_ex(('127.0.0.1', port)) != 0
        assert len(state.provider) == 4
        print(json.dumps({'passed': True, 'cases': results, 'provider_calls': len(state.provider), 'provider': 'offline mock'}))
    finally:
        provider.shutdown()
        provider.server_close()


if __name__ == '__main__':
    main()
