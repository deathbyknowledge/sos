from dataclasses import Field
import os
import art
from rich import print
from typing import List
from pydantic import BaseModel
from tenacity import retry, stop_after_attempt
from sos import SoSClient
from project_types import Scenario, Message

from dotenv import load_dotenv

load_dotenv()

SHELLM= os.getenv("SHELLM", "1") == "1"
EPHEMERAL = os.getenv("EPHEMERAL", "1") == "1"
MAX_TURNS = int(os.getenv("MAX_TURNS", "30"))  # reasoning counts as 1 turn
MAX_MODEL_TOKENS = int(os.getenv("MAX_MODEL_TOKENS", "32000"))
sos = SoSClient(server_url="http://localhost:3000")


class ShellTrajectory(art.Trajectory):
    task_id: str
    sandbox_id: str
    exit_codes: List[int]
    success_condition_passed: bool
    corrupted: bool
    metrics: dict[str, float] = {}


class RolloutConfig(BaseModel):
    max_turns: int = 30
    max_tokens: int = 512
    temperature: float = 1.0


async def rollout(
    model: art.Model, scenario: Scenario, config: RolloutConfig = RolloutConfig()
) -> ShellTrajectory:
    client = model.openai_client()
    sandbox_id = await sos.create_sandbox(
        image="deathbyknowledge/shellm-sandbox:latest",
        setup_commands=scenario.setup_commands,
    )
    traj = ShellTrajectory(
        reward=0.0,
        messages_and_choices=[],
        task_id=scenario.id,
        sandbox_id=sandbox_id,
        exit_codes=[],
        success_condition_passed=False,
        corrupted=False,
    )

    system_prompt = f"""
    [SHELL MODE]
    Shell mode is enabled. User streams have been replaced with standard output and standard error streams.
    Outputs are not streamed to the user but piped directly to standard input. For reasoning traces, you can prepend with `#` so the shell treats them as comments.
    On startup, standard output will print the task. To signal the completion of your run, use `exit 0`.

    TASK: {scenario.task}
    """

    traj.messages_and_choices = [
        {"role": "system", "content": scenario.task if SHELLM else system_prompt},
    ]
    traj.exit_codes = []

    await sos.start_sandbox(sandbox_id)

    async def finish_traj(sandbox_id: str, success_command: str) -> bool:
        try:
            _, code, _ = await sos.exec_command(
                sandbox_id, success_command, standalone=True
            )
            await sos.stop_sandbox(sandbox_id, remove=EPHEMERAL)
            return code == 0
        except Exception as e:
            await sos.stop_sandbox(sandbox_id, remove=EPHEMERAL)
            print(f"[ {scenario.id} ] Error running success command in sandbox: {e}")
            return False

    for _ in range(MAX_TURNS):

        @retry(stop=stop_after_attempt(3))
        async def get_response():
            response = await client.chat.completions.create(
                messages=traj.messages(),
                model=model.name,
                temperature=config.temperature,
                max_tokens=config.max_tokens,
            )

            if (
                not response.choices[0].message.content
                or response.choices[0].message.content is None
            ):
                raise Exception("No response from model")

            if response.usage.completion_tokens == 512:
                traj.corrupted = True
                response.choices[0].message.content = "exit 0"

            return response.choices[0]

        # TODO: This is a hack to estimate the token count. Find how to get
        # the tokenizer from the ART model or get it manually.
        approx_token_count = 0
        for msg in traj.messages():
            approx_token_count += len(msg["content"]) / 3
        if approx_token_count > MAX_MODEL_TOKENS:
            await finish_traj(sandbox_id, scenario.success_condition)
            traj.success_condition_passed = False
            return traj

        response_message = await get_response()

        traj.messages_and_choices.append(response_message)

        cmd = response_message.message.content

        try:
            output, exit_code, exited = await sos.exec_command(sandbox_id, cmd)

            traj.messages_and_choices.append({"role": "user", "content": output})

            if exited:
                # It's over
                if len(cmd) > len("exit 0"):
                    print(f"Exit cmd: {cmd}")
                break

            if exit_code != -1:
                traj.exit_codes.append(exit_code)
            else:
                print("-1 exit code detected")

        except Exception as e:
            print(f"Error running command in sandbox: {e}")
            output = f"Error running command: {e}"
            traj.messages_and_choices.append({"role": "user", "content": output})
            traj.exit_codes.append(-1)
            await finish_traj(sandbox_id, scenario.success_condition)
            traj.success_condition_passed = False
            traj.corrupted = False
            return traj

    condition_passed = await finish_traj(sandbox_id, scenario.success_condition)
    traj.success_condition_passed = condition_passed
    return traj


async def rollout_and_score(
    model: art.Model, scenario: Scenario, config: RolloutConfig = RolloutConfig()
) -> ShellTrajectory:
    traj = await rollout(model, scenario, config=config)

    """
    success ∈ [0, 1], T = num_turns
    L(T) = 1 / (1 + exp((T - T0) / tau)) (length factor in [0, 1])
    e = #{non-zero exit codes} / max(1, T) (error rate in [0, 1])
    R = success * (1 - alpha_len * (1 - L(T))) - alpha_exit * e  - (1 - success) * eps_fail * T
        ^ success shaped by turns                ^ exit penalty     ^ fail per-turn penalty
    """

    def length_factor_logistic(T, T0=15.0, tau=2.5):
        import math

        return 1.0 / (1.0 + math.exp((T - T0) / tau))

    def reward_from_outcome(
        success: bool,
        turns: int,
        nonzero_exit_count: int,
        alpha_len=0.4,
        alpha_exit=0.1,
        eps_fail=0.01,
        T0=15.0,
        tau=2.5,
    ):
        T = max(1, int(turns))
        L = length_factor_logistic(T, T0=T0, tau=tau)
        e_rate = min(1.0, nonzero_exit_count / T)

        success_term = (1.0 - alpha_len * (1.0 - L)) if success else 0.0
        exit_penalty = alpha_exit * e_rate
        fail_penalty = 0 if success else eps_fail * T
        traj.metrics["length_factor"] = L
        traj.metrics["success_term"] = success_term
        traj.metrics["exit_penalty"] = exit_penalty
        traj.metrics["fail_penalty"] = fail_penalty
        traj.metrics["exit_error_rate"] = e_rate
        traj.metrics["success"] = float(success)
        traj.metrics["turns"] = float(T)
        traj.metrics["nonzero_exit_codes"] = float(nonzero_exit_count)
        R = success_term - exit_penalty - fail_penalty

        return max(-1.0, min(1.0, R))

    num_turns = len([m for m in traj.messages() if m["role"] == "assistant"])
    nonzero_exits = sum(1 for c in traj.exit_codes if c != 0)
    try:
        reward = reward_from_outcome(
            traj.success_condition_passed, num_turns, nonzero_exits
        )
    except Exception as e:
        traj.corrupted = True
        reward = 0.0
    traj.reward = reward
    return traj


if __name__ == "__main__":
    import asyncio
    from load_scenarios import load_scenarios

    scenario = load_scenarios(limit=1)[0]
    model_name = os.getenv("MODEL", "deathbyknowledge/Qwen2.5-7B-Shell-SFT")
    model = art.Model(
        name=model_name,
        project="shell-agent-test",
        inference_api_key=os.getenv("INFERENCE_API_KEY", "FAKE_KEY"),
        inference_base_url=os.getenv("INFERENCE_BASE_URL", "http://localhost:8000/v1"),
        inference_model_name=model_name,
    )
    traj = asyncio.run(rollout_and_score(model, scenario))
    print(traj.reward)
