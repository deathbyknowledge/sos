from project_types import Scenario
from typing import Optional, Literal
from datasets import load_dataset, Dataset
import random

HF_REPO_ID = "deathbyknowledge/shell-tasks"

def load_scenarios(
  split: Literal["train", "test"] = "train",
  limit: Optional[int] = None,
  shuffle: bool = False,
  seed: Optional[int] = None,
  difficulty: Optional[Literal[1, 2, 3, 4]] = None,
):
    dataset: Dataset = load_dataset(HF_REPO_ID, split=split)

    if difficulty is not None:
        dataset = dataset.filter(lambda x: x["difficulty_level"] == difficulty)

    if shuffle or (seed is not None):
        if seed is not None:
            dataset = dataset.shuffle(seed=seed)
        else:
            dataset = dataset.shuffle()

    # Convert each row (dict) in the dataset to a Scenario object
    scenarios = [
        Scenario(id=row['task_id'], task=row['task'], # type: ignore
                setup_commands=row['setup_commands'],success_condition=row['success_condition'])  # type: ignore
        for row in dataset  # type: ignore
    ]

    if shuffle:
        if seed is not None:
            rng = random.Random(seed)
            rng.shuffle(scenarios)
        else:
            random.shuffle(scenarios)

    if limit is not None:
        return scenarios[:limit]
    else:
        return scenarios
