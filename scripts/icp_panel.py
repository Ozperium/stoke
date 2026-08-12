#!/usr/bin/env python3
"""Score the current Stoke landing and product through four local-Gemma role-based ICPs."""

import argparse
import json
from pathlib import Path
from urllib.error import URLError
from urllib.request import Request, urlopen

ROOT = Path(__file__).resolve().parents[1]
MODEL = "gemma4:12b-mlx"
OLLAMA_URL = "http://127.0.0.1:11434/api/generate"

SCHEMA = {
    "type": "object",
    "required": ["persona", "scores", "blockers", "single_next_change"],
    "properties": {
        "persona": {"type": "string"},
        "scores": {
            "type": "object",
            "required": ["landing", "product", "grand_total"],
            "properties": {
                "landing": {
                    "type": "object",
                    "required": ["problem_and_outcome", "clarity_and_activation", "trust_and_proof", "model_supply_and_first_value", "total"],
                    "properties": {
                        "problem_and_outcome": {"type": "integer", "minimum": 0, "maximum": 20},
                        "clarity_and_activation": {"type": "integer", "minimum": 0, "maximum": 20},
                        "trust_and_proof": {"type": "integer", "minimum": 0, "maximum": 40},
                        "model_supply_and_first_value": {"type": "integer", "minimum": 0, "maximum": 20},
                        "total": {"type": "integer", "minimum": 0, "maximum": 100},
                    },
                },
                "product": {
                    "type": "object",
                    "required": ["job_coverage", "operational_fit", "safety_and_proof", "total"],
                    "properties": {
                        "job_coverage": {"type": "integer", "minimum": 0, "maximum": 30},
                        "operational_fit": {"type": "integer", "minimum": 0, "maximum": 25},
                        "safety_and_proof": {"type": "integer", "minimum": 0, "maximum": 25},
                        "total": {"type": "integer", "minimum": 0, "maximum": 80},
                    },
                },
                "grand_total": {"type": "integer", "minimum": 0, "maximum": 180},
            },
        },
        "blockers": {"type": "array", "items": {"type": "string"}, "maxItems": 4},
        "single_next_change": {"type": "string"},
        "critique": {"type": "string"},
    },
}


def read(path: str) -> str:
    return (ROOT / path).read_text()


def prompt_for(profile: str, dossier: str) -> str:
    return f"""You are personifying this project-supplied ideal customer profile:\n\n{profile}\n\nBe a hard critic, not a helpful marketer. Score only the supplied current evidence. Do not invent product functionality, market proof, certifications, integrations, customer interviews, or roadmap behavior. Missing evidence earns zero for that criterion.\n\nScore landing out of 100: problem/outcome 20, clarity/activation 20, trust/proof 40, model supply and first value 20. The model-supply criterion must answer: what serves the next request, how capacity is attached in minutes, whether unapproved sources can be prohibited, whether the serving source is visible, and what happens when no eligible source exists. Score product out of 80: job coverage 30, operational fit 25, safety/proof 25. Total must equal landing + product. The raised gate requires landing >=90, product >=80, and total >=170 for every persona; one surface cannot compensate for the other. Apply disqualifiers and cap/fail a surface when a must-see boundary is absent. Do not round up to pass.\n\nName the few highest-impact blockers. `single_next_change` must be exactly one smallest truthful product or landing change; do not recommend broad platforms, certifications, or generic marketing. Return JSON matching the requested schema.\n\nCURRENT EVIDENCE:\n{dossier}"""


def decode_score(text: str, persona: str) -> dict:
    text = text.strip().removeprefix("```json").removeprefix("```").removesuffix("```").strip()
    try:
        raw, _ = json.JSONDecoder().raw_decode(text[text.index("{"):])
    except (ValueError, json.JSONDecodeError) as error:
        raise SystemExit(f"Gemma did not return score JSON: {text[:500]}") from error

    nested = raw.get("score")
    score = nested if isinstance(nested, dict) else raw.get("scorecard", raw.get("breakdown", raw))
    if isinstance(raw.get("scores"), dict):
        score = raw["scores"]
    elif isinstance(raw.get("persona_score"), dict):
        score = raw["persona_score"]
    if not isinstance(score, dict):
        score = raw

    def subscore(value: object, named: str) -> int | None:
        if isinstance(value, int):
            return value
        if not isinstance(value, dict):
            return None
        for key in ("score", "total", f"total_{named}"):
            if isinstance(value.get(key), int):
                return value[key]
        components = [item for item in value.values() if isinstance(item, int)]
        return sum(components) if components else None

    landing_raw = score.get("landing", raw.get("landing", {}))
    product_raw = score.get("product", raw.get("product", {}))
    landing = subscore(landing_raw, "landing")
    product = subscore(product_raw, "product")
    landing = score.get("landing_score", raw.get("landing_score", landing)) if landing is None else landing
    product = score.get("product_score", raw.get("product_score", product)) if product is None else product
    if not isinstance(landing, int) or not isinstance(product, int) or not 0 <= landing <= 100 or not 0 <= product <= 80:
        raise SystemExit(f"Invalid score shape: {json.dumps(raw)}")
    critique = raw.get("critique", score.get("critique", ""))
    blockers = raw.get("blockers", score.get("blockers", []))
    single_next_change = raw.get("single_next_change", score.get("single_next_change", raw.get("recommended_change", "Not supplied")))
    reasons = [critique] if critique else []
    return {
        "persona": raw.get("persona", persona),
        "landing": {"score": landing, "reasons": landing_raw.get("reasons", reasons) if isinstance(landing_raw, dict) else reasons},
        "product": {"score": product, "reasons": product_raw.get("reasons", reasons) if isinstance(product_raw, dict) else reasons},
        "total": landing + product,
        "blockers": blockers,
        "single_next_change": single_next_change,
        "critique": critique,
    }


def run(prompt: str, model: str, persona: str) -> dict:
    payload = json.dumps({
        "model": model,
        "prompt": prompt,
        "stream": False,
        "format": SCHEMA,
        "think": False,
        "options": {"temperature": 0, "num_predict": 2048},
        "keep_alive": "10m",
    }).encode()
    request = Request(OLLAMA_URL, data=payload, headers={"Content-Type": "application/json"})
    try:
        with urlopen(request, timeout=600) as response:
            body = json.loads(response.read())
    except URLError as error:
        raise SystemExit(f"Ollama request failed: {error}") from error
    response_text = body.get("response", "")
    if not response_text:
        raise SystemExit(f"Ollama returned no score response: {json.dumps(body)[:1000]}")
    return decode_score(response_text, persona)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--model", default=MODEL)
    parser.add_argument("--dry-run", action="store_true")
    parser.add_argument("--profile", type=int, help="Run only the 1-based profile number")
    args = parser.parse_args()

    panel = read("docs/icp-scorecard.md")
    rubric = panel[:panel.index("## Profile")]
    profiles = panel.split("## Profile ")[1:]
    if args.profile is not None:
        if not 1 <= args.profile <= len(profiles):
            parser.error(f"--profile must be between 1 and {len(profiles)}")
        profiles = [profiles[args.profile - 1]]
    landing = read("landing/index.html")
    readme = read("README.md")
    spend_smoke_lines = read("scripts/smoke_spend.sh").splitlines()
    spend_proof = "\n".join(spend_smoke_lines[145:378] + spend_smoke_lines[421:465])
    dossier = "\n\n".join((
        "SCORING RULES:\n" + rubric,
        "LANDING:\n" + landing[landing.index("<!-- 1. Hero -->"):landing.index("<!-- 12. Footer -->")],
        "README:\n" + readme[:readme.index("## Quickstart")],
        "CONFIG:\n" + read("stoke.example.toml"),
        "RUNNABLE SPEND PROOF:\n" + spend_proof,
        "STATIC CHECKS:\n" + read("scripts/check_landing.py"),
    ))

    if args.dry_run:
        print(json.dumps({"model": args.model, "profiles": len(profiles), "evidence_chars": len(dossier)}, indent=2))
        return

    scores = [
        run(prompt_for(profile, dossier), args.model, profile.splitlines()[0].strip())
        for profile in profiles
    ]
    passed = all(score["total"] >= 170 and score["landing"]["score"] >= 90 and score["product"]["score"] >= 80 for score in scores)
    print(json.dumps({"model": args.model, "gate": "pass" if passed else "fail", "scores": scores}, indent=2))


if __name__ == "__main__":
    main()
