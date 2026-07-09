#!/bin/bash
cd "$(dirname "$0")"

echo "=== qwen2.5-coder:7b ==="
python3 run_benchmark.py --model qwen2.5-coder:7b --routing single --limit 164 --workers 4 --output results_full_qwen25_coder_7b.json

echo "=== qwen2.5-coder:3b ==="
python3 run_benchmark.py --model qwen2.5-coder:3b --routing single --limit 164 --workers 4 --output results_full_qwen25_coder_3b.json

echo "=== qwen3:8b ==="
python3 run_benchmark.py --model qwen3:8b --routing single --limit 164 --workers 4 --output results_full_qwen3_8b.json

echo "=== phi4-mini ==="
python3 run_benchmark.py --model phi4-mini --routing single --limit 164 --workers 4 --output results_full_phi4_mini.json

echo "=== llama3.2:3b ==="
python3 run_benchmark.py --model llama3.2:3b --routing single --limit 164 --workers 4 --output results_full_llama32_3b.json

echo "=== test_vote small models ==="
python3 run_benchmark.py --model qwen2.5-coder:7b --routing test_vote --vote-models qwen2.5-coder:7b,qwen2.5-coder:3b,qwen3:8b,phi4-mini,llama3.2:3b --limit 164 --workers 2 --output results_full_test_vote_5small.json

echo "=== test_vote best small + best big ==="
python3 run_benchmark.py --model gpt-oss:20b --routing test_vote --vote-models gpt-oss:20b,qwen2.5-coder:7b,qwen3:8b --limit 164 --workers 2 --output results_full_test_vote_mixed3.json

echo "ALL SMALL MODELS DONE"
