import art
import asyncio
from rollout import RolloutConfig, rollout_and_score, ShellTrajectory
from load_scenarios import load_scenarios
from tqdm.asyncio import tqdm
import os


async def benchmark(
    model: art.Model, num_scenarios: int, difficulty: None | int = None
) -> tuple[list[ShellTrajectory], float]:
    scenarios = load_scenarios(limit=num_scenarios, split="test", difficulty=difficulty)
    results: list[ShellTrajectory] = await tqdm.gather(
        *[
            rollout_and_score(model, scenario, config=RolloutConfig(temperature=0.0))
            for scenario in scenarios
        ],
        desc=f"benchmarking {model.name}",
    )
    scores = [result.reward for result in results]
    accuracy = sum([result.success_condition_passed for result in results]) / len(
        results
    )
    return results, sum(scores) / len(scores) if scores else 0, accuracy


async def benchmark_all_models(
    num_scenarios: int, difficulty: None | int = None
) -> dict[str, float]:
    model_names = [
        "deathbyknowledge/Qwen2.5-7B-Shell-SFT",
    ]

    models = [
        art.Model(
            name=name,
            project="shell-agent-test",
            inference_api_key=os.getenv("INFERENCE_API_KEY", "FAKE_KEY"),
            inference_base_url=os.getenv(
                "INFERENCE_BASE_URL", "http://localhost:8000/v1"
            ),
        )
        for name in model_names
    ]
    results = await asyncio.gather(
        *[benchmark(model, num_scenarios, difficulty) for model in models]
    )
    return {
        model.name: {"score": score, "accuracy": accuracy}
        for model, (_results, score, accuracy) in zip(models, results)
    }


if __name__ == "__main__":
    results = asyncio.run(benchmark_all_models(num_scenarios=20))
    print(results)
