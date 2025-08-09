import art
from rollout import rollout_and_score
from load_scenarios import load_scenarios
from art.local import LocalBackend
from art.utils import iterate_dataset
from project_types import RunConfig
from benchmark import benchmark


async def train(model: art.TrainableModel[RunConfig]):
    training_data = load_scenarios(split="train", limit=model.config.training_num_scenarios, difficulty=model.config.difficulty)

    with LocalBackend() as backend:
        await model.register(backend)

        training_iterator = iterate_dataset(
            training_data,
            groups_per_step=model.config.groups_per_step,
            num_epochs=model.config.num_epochs,
            initial_step=await model.get_step(),
        )

        for dataset_batch in training_iterator:
            batch = dataset_batch.items
            global_step = dataset_batch.step
            
            if global_step % model.config.validation_frequency == 0:
                results, score, accuracy = await benchmark(
                    model, model.config.validation_num_scenarios, difficulty=model.config.difficulty
                )
                await model.log(results)
            groups = []
            for scenario in batch:
                groups.append(
                    art.TrajectoryGroup(
                        (
                            rollout_and_score(model, scenario)
                            for _ in range(model.config.rollouts_per_group)
                        )
                    )
                )
            finished_groups = await art.gather_trajectory_groups(groups)

            # Filter out corrupted trajectories from groups
            filtered_groups = []
            for group in finished_groups:
                filtered_group = []
                for traj in group:
                    if traj.corrupted: # type: ignore
                        continue
                    filtered_group.append(traj)
                filtered_groups.append(art.TrajectoryGroup(filtered_group))

            await model.train(
                filtered_groups,
                config=art.TrainConfig(learning_rate=model.config.learning_rate),
            )

if __name__ == "__main__":
    import asyncio
    from all_experiments import models
    import argparse

    parser = argparse.ArgumentParser(description="Train a model")
    parser.add_argument(
        "--model",
        required=True,
        help="The key of the model to train as defined in all_experiments.py (e.g. 'run_1')",
    )
    args = parser.parse_args()
    model = models[args.model]
    asyncio.run(train(model))