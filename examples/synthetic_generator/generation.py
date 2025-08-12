# /// script
# dependencies = [
#   "openai>=1.6.0",
#   "datasets>=3.6.0",
#   "tenacity>=8.0.0",
#   "httpx>=0.27.0",
#   "rich>=13.9.5",
# ]
# requires-python = ">=3.10"
# ///

"""
https://arxiv.org/pdf/2507.23751

The paper uses a single model to generate and solve tasks (based on some seed tasks), to later train the same model on them.
Instead, I use their method to generate shell tasks so we can later run RL on smaller models with the generated tasks.

We use SoS to validate the tasks. This script does the following:
  1. Generate a task based on two seed tasks
  2. Start a sandbox, run the setup commands  and validate it returns 0 exit code
  3. Run the success condition command and validate it does NOT return 0 exit code (i.e. success condition fails when no actions are taken)
  4. Now that the setup is validates, we generate K rollouts (K concurrent solves) and validate if the success condition passes in at least T of them
  5. If the success condition passes in at least T of them, we save the task and the rollouts
"""

import os
import re
import json
import uuid
import asyncio
import httpx
from pathlib import Path
from datetime import datetime, timezone
from typing import Any, Dict, List, Tuple, Optional
from rich import print

from openai import AsyncOpenAI, BaseModel
from datasets import load_dataset, Dataset
from tenacity import retry, stop_after_attempt
from sos import SoSClient

GENERATION_TEMPLATE = """
You are a shell task generator assistant. Your goal is to create a novel and challenging shell task for an Ubuntu-like sandbox environment.

Please follow the steps below to create a synthetic shell task.

Step 1: Carefully read Seed Task 1 and Seed Task 2. Identify and list all common elements between these tasks (e.g., types of operations, environment assumptions, verification style). If no common elements are found, list the main elements from each task.

Step 2: Develop a comprehensive plan based on the Common Elements List or Main Elements List from Step 1. This plan will guide the generation of a new synthetic shell task that is similar in quality and complexity to the original tasks, including a task description, setup commands, and success condition. Ensure:
   - The setup commands prepare the environment (e.g., create files/directories) and execute successfully.
   - The success condition is a single bash command that returns exit code 0 if the task is completed correctly and non-zero initially (before agent actions).
   - The task is novel, inspired by but not copying the seeds.

Step 3: Execute the plan step by step and provide the new synthetic task components.

Seed Task 1:
Task Description: {task_1}
Setup Commands: {setup_1}
Success Condition: {success_1}

Seed Task 2:
Task Description: {task_2}
Setup Commands: {setup_2}
Success Condition: {success_2}

Please reply strictly in the following format:
- Step 1: #Common Elements List#
- Step 2: #Plan#
- Step 3: #Task Description#: [description]
  #Setup Commands#: [setup commands, separated by semicolons]
  #Success Condition#: [success command]
"""

SOLVER_TEMPLATE = """
You are an expert Linux shell user. Your goal is to complete the given task step-by-step.
Given a task and the history of previous turns, provide your reasoning in a 'thought' process
and then provide the single, next shell command to execute as the 'command'.
Pay attention to the output of the previous commands. If it is an error, you should provide a
debugging command to fix the issue and complete the previous turn's goal.
The 'reasoning', while it can be as verbose as you want, should not include newline `
` characters.
The 'command' should be a valid shell command to input in the terminal.
If the task is complete, the command should be exactly 'exit 0'.

TASK: {task}
SHELL HISTORY:
{history}

Please reply strictly in the following format:
  #Reasoning#: [reasoning]
  #Command#: [command]
"""


class ShellTask(BaseModel):
    task: str
    setup_commands: str
    success_condition: str
    difficulty_level: int


def short_id() -> str:
    """uuid4 first 8 hex chars."""
    return uuid.uuid4().hex[:8]


def utc_now_iso() -> str:
    return datetime.now(timezone.utc).isoformat()


def append_jsonl(path: Path, obj: Dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("a", encoding="utf-8") as f:
        f.write(json.dumps(obj, ensure_ascii=False) + "\n")


def load_jsonl(path: Path) -> List[Dict[str, Any]]:
    if not path.exists():
        return []
    with path.open("r", encoding="utf-8") as f:
        return [json.loads(line) for line in f if line.strip()]


def normalize_task_text(s: str) -> str:
    return " ".join(s.split()).lower()


def get_seed_tasks(dataset: Dataset, difficulty: int, offset: int = 0) -> List[ShellTask]:
    tasks = dataset.filter(lambda x: x["difficulty_level"] == difficulty)
    tasks = tasks.select(range(offset, offset + 2)) if offset + 2 <= tasks.num_rows else []
    return [
        ShellTask(
            task=x["task"],
            setup_commands="; ".join(x["setup_commands"]),
            success_condition=x["success_condition"],
            difficulty_level=difficulty,
        )
        for x in tasks
    ]


@retry(stop=stop_after_attempt(3))
async def generate_task(
    task_1: ShellTask,
    task_2: ShellTask,
    difficulty_level: int,
    *,
    temperature: float = 1.0,
) -> ShellTask:
    async def generate(template: str) -> str:
        messages = [{"role": "user", "content": template}]
        response = await client.chat.completions.create(
            model=MODEL,
            messages=messages,
            temperature=temperature,
        )
        return response.choices[0].message.content or ""

    def _extract(pattern: str, text: str) -> Optional[str]:
        m = re.search(pattern, text, re.DOTALL)
        return m.group(1).strip() if m else None

    template = GENERATION_TEMPLATE.format(
        task_1=task_1.task,
        setup_1=task_1.setup_commands,
        success_1=task_1.success_condition,
        task_2=task_2.task,
        setup_2=task_2.setup_commands,
        success_2=task_2.success_condition,
    )

    response = await generate(template)

    task = _extract(r"#Task Description#:(.*?)#Setup Commands#", response)
    setup_commands = _extract(r"#Setup Commands#:(.*?)#Success Condition#", response)
    success_condition = _extract(r"#Success Condition#:(.*)$", response)

    if not task or not setup_commands or not success_condition:
        raise ValueError("Failed to parse task from response")

    return ShellTask(
        task=task,
        setup_commands=setup_commands,
        success_condition=success_condition,
        difficulty_level=difficulty_level,
    )


async def validate_task_setup(task: ShellTask) -> bool:
    """Validate setup commands run and success condition fails initially."""
    sid = await sos.create_sandbox(image="deathbyknowledge/shellm-sandbox:latest", setup_commands=[task.setup_commands])
    try:
        await sos.start_sandbox(sid)  # Runs the setup commands, will raise if they fail
        _, exit_code, _ = await sos.exec_command(sid, task.success_condition, standalone=True)
        return exit_code != 0  # Expect failure before agent acts
    except Exception as e:
        print("setup validation error:", e)
        return False
    finally:
        await sos.stop_sandbox(sid, remove=True)


async def validate_task_success(
    task: ShellTask, k: int = 4, threshold: float = 0.5
) -> Tuple[bool, List[Dict[str, Any]]]:
    """
    Run K concurrent solve attempts and check if the fraction of successes >= threshold.
    Returns: (passed_threshold, rollouts)
    rollouts = [{"solved": bool, "history": str}, ...]
    """
    attempts = [solve(task) for _ in range(k)]
    results = await asyncio.gather(*attempts, return_exceptions=True)

    rollouts: List[Dict[str, Any]] = []
    successes = 0

    for res in results:
        if isinstance(res, Exception):
            rollouts.append({"solved": False, "history": f"Exception: {res}"})
        else:
            solved, history = res
            if solved:
                successes += 1
            rollouts.append({"solved": solved, "history": history})

    passed = (successes / max(k, 1)) >= threshold
    return passed, rollouts


async def solve(task: ShellTask) -> Tuple[bool, str]:
    def parse_reasoning_from_response(response: str) -> Optional[str]:
        m = re.search(r"#Reasoning#:(.*)#Command#", response, re.DOTALL)
        return m.group(1).strip() if m else None

    def parse_command_from_response(response: str) -> Optional[str]:
        m = re.search(r"#Command#:(.*)$", response, re.DOTALL)
        return m.group(1).strip() if m else None

    @retry(stop=stop_after_attempt(3))
    async def generate(template: str) -> Tuple[str, str]:
        messages = [{"role": "user", "content": template}]
        response = await client.chat.completions.create(
            model=MODEL,
            messages=messages,
            temperature=0.7,
        )
        message = response.choices[0].message.content
        if not message:
            raise ValueError("Failed to generate response")
        reasoning = parse_reasoning_from_response(message)
        command = parse_command_from_response(message)
        if reasoning is None or command is None:
            raise ValueError("Failed to parse reasoning or command from response")
        return reasoning, command

    turn = 0
    MAX_TURNS = 15
    sid = await sos.create_sandbox(image="deathbyknowledge/shellm-sandbox:latest", setup_commands=[task.setup_commands])
    solved = False
    history = "First turn, no history available yet."
    try:
        await sos.start_sandbox(sid)
        while turn < MAX_TURNS:
            turn += 1
            template = SOLVER_TEMPLATE.format(task=task.task, history=history)
            reasoning, command = await generate(template)
            # Store reasoning as a comment line to keep a complete trajectory in the sandbox logs
            _, _, _ = await sos.exec_command(sid, f"# {reasoning}")
            _, _, exited = await sos.exec_command(sid, command)
            history = await sos.get_sandbox_trajectory(sid, formatted=True)
            if exited:
                break
    except Exception as e:
        print("solver error:", e)
    finally:
        # history as JSON string for distillation (formatted=False)
        traj = await sos.get_sandbox_trajectory(sid, formatted=False)
        history = traj['trajectory']
        _, exit_code, _ = await sos.exec_command(sid, task.success_condition, standalone=True)
        solved = exit_code == 0
        await sos.stop_sandbox(sid, remove=False)
    return solved, history


async def process_seed_pair(
    seeds: List[ShellTask],
    difficulty: int,
    k: int,
    threshold: float,
    save_all_rollouts: bool,
    tasks_path: Path,
    rollouts_path: Path,
    seen_texts: set[str],
    counts_by_diff: Dict[int, int],
    *,
    candidates_per_pair: int = 1,
    gen_temperature: float = 1.0,
) -> bool:
    """Process one seed pair into up to N validated + persisted candidates. Returns True if saved at least one."""

    try:
        candidate = await generate_task(
            seeds[0], seeds[1], difficulty,
            temperature=gen_temperature,
        )
    except Exception as e:
        print("generation error:", e)
        return False

    nt = normalize_task_text(candidate.task)

    # Reserve this task text to avoid duplicate work across workers
    if nt in seen_texts:
        print("duplicate task text, skipping")
        return False
    seen_texts.add(nt)

    # Validate setup
    if not await validate_task_setup(candidate):
        print("setup invalid, skipping")
        seen_texts.discard(nt)
        return False

    # Validate solve-ability
    passed, rollouts = await validate_task_success(candidate, k=k, threshold=threshold)
    if not passed:
        print("did not pass success threshold, skipping")
        seen_texts.discard(nt)

    # Persist (single-writer section)
    task_id = short_id()
    task_record = {
        "id": task_id,
        "task": candidate.task,
        "setup_commands": [candidate.setup_commands],
        "success_condition": candidate.success_condition,
        "difficulty_level": candidate.difficulty_level,
        "k": k,
        "threshold": threshold,
        "timestamp": utc_now_iso(),
    }

    append_jsonl(tasks_path, task_record)
    counts_by_diff[difficulty] = counts_by_diff.get(difficulty, 0) + 1
    # Persist rollouts (all or only successful)
    for i, r in enumerate(rollouts):
        if save_all_rollouts or r.get("solved"):
            rollout_record = {
                "id": short_id(),
                "task_id": task_id,
                "attempt": i,
                "solved": bool(r.get("solved")),
                # history is a JSON string from SoS (formatted=False above)
                "history": r.get("history", ""),
                "difficulty_level": difficulty,
                "timestamp": utc_now_iso(),
            }
            append_jsonl(rollouts_path, rollout_record)

    print(f"saved task {task_id} (difficulty {difficulty}); total now {counts_by_diff[difficulty]}.")
    saved_any = True

    return saved_any


async def run_pipeline(
    out_dir: str = "out",
    tasks_per_difficulty: int = 3,
    difficulties: Optional[List[int]] = None,
    k: int = 4,
    threshold: float = 0.5,
    seed_step: int = 2,
    save_all_rollouts: bool = False,
) -> None:
    out = Path(out_dir)
    tasks_path = out / "generated_tasks.jsonl"
    rollouts_path = out / "rollouts.jsonl"

    # Load existing tasks and build fast lookups
    existing_tasks = load_jsonl(tasks_path)
    counts_by_diff: Dict[int, int] = {}
    seen_texts: set[str] = set()
    for rec in existing_tasks:
        d = int(rec.get("difficulty_level", 0))
        counts_by_diff[d] = counts_by_diff.get(d, 0) + 1
        t = rec.get("task", "")
        if t:
            seen_texts.add(normalize_task_text(t))

    # discover difficulty levels if not provided
    if difficulties is None:
        try:
            difficulties = sorted(set(dataset["difficulty_level"]))
        except Exception:
            difficulties = [1, 2, 3, 4]

    # This is an example script, you'll want to parallelize this
    for difficulty in difficulties:
        target = tasks_per_difficulty
        have = counts_by_diff.get(difficulty, 0)
        if have >= target:
            print(f"=== Difficulty {difficulty}: already have {have}/{target}, skipping ===")
            continue

        print(f"=== Difficulty {difficulty}: {have}/{target} so far ===")
        offset = 0

        # Keep going until we reach target for this difficulty
        while counts_by_diff.get(difficulty, 0) < target:
            seeds = get_seed_tasks(dataset, difficulty, offset)
            if len(seeds) < 2:
                print(f"No more seed pairs at difficulty {difficulty} (offset={offset}).")
                break
            await process_seed_pair(
                seeds, difficulty, k, threshold, save_all_rollouts,
                tasks_path, rollouts_path, seen_texts, counts_by_diff,
            )
            offset += seed_step  # advance regardless to avoid getting stuck


BASE_URL = os.getenv("BASE_URL", "https://api.deepseek.com/v1")
API_KEY = os.getenv("API_KEY")
MODEL = "deepseek-chat"
client = AsyncOpenAI(
    base_url=BASE_URL,
    api_key=API_KEY,
    http_client=httpx.AsyncClient(timeout=60.0),
)
client = AsyncOpenAI(base_url=BASE_URL, api_key=API_KEY)

dataset = load_dataset("deathbyknowledge/shell-tasks", split="train")
dataset = Dataset.from_list(dataset)
sos = SoSClient(server_url="http://localhost:3000")

"""
API_KEY="YOUR_KEY" BASE_URL="https://api.deepseek.com/v1" uv run examples/synthetic_generator/generation.py


Update the dataset to set custom seed tasks.
"""

# TODO: dedup
if __name__ == "__main__":
    async def main():
        await run_pipeline(
            out_dir="data",
            tasks_per_difficulty=2,
            difficulties=[1, 2],
            k=4,
            threshold=0.5,
            seed_step=1,
            save_all_rollouts=True,
        )

    asyncio.run(main())