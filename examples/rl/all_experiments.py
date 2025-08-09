import art
from project_types import RunConfig


models: dict[str, art.TrainableModel[RunConfig]] = {}

models["run_1"] = art.TrainableModel[RunConfig](
    base_model="deathbyknowledge/Qwen2.5-3B-Shell-SFT",
    project="shell-agent",
    name="run_1",
    config=RunConfig(),
)

models["run_2"] = models["run_1"].model_copy(deep=True)
models["run_2"].name = "run_2"
models["run_2"].config.groups_per_step = 8

models["run_3"] = models["run_1"].model_copy(deep=True)
models["run_3"].name = "run_3"
models["run_3"].config.groups_per_step = 8
models["run_3"].config.learning_rate = 5e-6


models["run_4"] = models["run_1"].model_copy(deep=True)
models["run_4"].name = "run_4"
models["run_4"].config.groups_per_step = 4
models["run_4"].config.rollouts_per_group = 12