import art
from project_types import RunConfig


models: dict[str, art.TrainableModel[RunConfig]] = {}

models["run_1"] = art.TrainableModel[RunConfig](
    base_model="deathbyknowledge/Qwen2.5-7B-Shell-SFT",
    project="sos-shellm-agent",
    name="run_1",
    config=RunConfig(
        groups_per_step=4,
        rollouts_per_group=8,
        difficulty=1,
        num_epochs=2,
        learning_rate=1e-5,
        validation_frequency=10,
        validation_num_scenarios=20,
        training_num_scenarios=508,
    ),
)

models["run_2"] = art.TrainableModel[RunConfig](
    base_model="deathbyknowledge/Qwen2.5-7B-Shell-SFT",
    project="sos-shellm-agent",
    name="run_2",
    config=RunConfig(
        groups_per_step=8,
        rollouts_per_group=8,
        difficulty=1,
        num_epochs=2,
        learning_rate=1e-5,
        validation_frequency=20,
        validation_num_scenarios=100,
        training_num_scenarios=508,
    ),
)

# with h200
models["run_3"] = art.TrainableModel[RunConfig](
    base_model="deathbyknowledge/Qwen2.5-7B-Shell-SFT",
    project="sos-shellm-agent",
    name="run_3",
    config=RunConfig(
        groups_per_step=12,
        rollouts_per_group=8,
        difficulty=None,
        num_epochs=2,
        learning_rate=1e-5,
        validation_frequency=50,
        validation_num_scenarios=100,
        training_num_scenarios=5000,
    ),
)