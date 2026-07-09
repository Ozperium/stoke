#!/bin/bash
# Run cloud model benchmarks on HumanEval
# 4 cloud models individually (20 problems each) + test_vote combination

set -e
cd "$(dirname "$0")"

echo "============================================"
echo "CLOUD MODEL BENCHMARKS - HumanEval 20"
echo "============================================"

# Run each cloud model individually
for model in "deepseek-v4-pro:cloud" "glm-5.2:cloud" "kimi-k2.6:cloud" "minimax-m3:cloud"; do
    echo ""
    echo "--- Running: $model ---"
    outfile="results_cloud_$(echo $model | tr ':' '_' | tr '/' '_')_20.json"
    python3 run_benchmark.py \
        --model "$model" \
        --routing single \
        --limit 20 \
        --workers 1 \
        --timeout 120 \
        --output "$outfile" 2>&1
done

# Run test_vote with all 4 cloud models
echo ""
echo "--- Running: test_vote (4 cloud models) ---"
python3 run_benchmark.py \
    --model "deepseek-v4-pro:cloud" \
    --routing test_vote \
    --vote-models "deepseek-v4-pro:cloud,glm-5.2:cloud,kimi-k2.6:cloud,minimax-m3:cloud" \
    --limit 20 \
    --workers 1 \
    --timeout 300 \
    --output "results_cloud_test_vote_4cloud_20.json" 2>&1

echo ""
echo "============================================"
echo "ALL CLOUD BENCHMARKS DONE"
echo "============================================"