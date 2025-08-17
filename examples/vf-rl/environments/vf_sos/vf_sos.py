from copy import deepcopy
import re
from typing import Any, Literal, Optional, Tuple
from openai import AsyncOpenAI
import verifiers as vf
from datasets import Dataset, load_dataset
import httpx

TIMEOUT = 300


class SoSAPIError(Exception):
    def __init__(self, status_code: int, message: str, endpoint: str):
        super().__init__(f"HTTP {status_code} when calling {endpoint}: {message}")
        self.status_code = status_code
        self.message = message
        self.endpoint = endpoint


class SoSBadRequestError(SoSAPIError):
    pass


class SoSNotFoundError(SoSAPIError):
    pass


class SoSTimeoutError(SoSAPIError):
    pass


class SoSInternalServerError(SoSAPIError):
    pass


class SoSClient:
    def __init__(self, server_url="http://localhost:3000"):
        self.server_url = server_url
        try:
            with httpx.Client(timeout=5) as client:
                response = client.request("GET", f"{self.server_url}/health")
            if response.status_code != 200:
                raise SoSAPIError(response.status_code, response.text, "/health")
        except Exception as e:
            print(f"Error checking health. Is SoS server up?: {e}")
            raise e

    def _raise_api_error(
        self, response: httpx.Response, path: str, context: str | None = None
    ):
        status = response.status_code
        message = response.text.strip()
        if context:
            message = f"{context}: {message}"
        if status == 400:
            raise SoSBadRequestError(status, message, path)
        if status == 404:
            raise SoSNotFoundError(status, message, path)
        if status == 504:
            raise SoSTimeoutError(status, message, path)
        if status >= 500:
            raise SoSInternalServerError(status, message, path)
        raise SoSAPIError(status, message, path)

    async def _request(
        self, method: str, path: str, json: dict | None = None
    ) -> httpx.Response:
        url = f"{self.server_url}{path}"
        async with httpx.AsyncClient(timeout=TIMEOUT) as client:
            response = await client.request(method, url, json=json)
        if response.status_code >= 400:
            self._raise_api_error(response, path)
        return response

    async def create_sandbox(self, image="ubuntu:latest", setup_commands=None):
        """
        Creates a new sandbox.

        Args:
            image (str): The container image to use.
            setup_commands (list): A list of commands to run after the container starts.
        Returns:
            str: The ID of the created sandbox.
        """
        if setup_commands is None:
            setup_commands = []
        payload = {"image": image, "setup_commands": setup_commands}
        response = await self._request("POST", "/sandboxes", json=payload)
        parsed = response.json()
        if "id" not in parsed:
            raise SoSAPIError(
                response.status_code, f"Missing id in response: {parsed}", "/sandboxes"
            )
        return parsed["id"]

    async def list_sandboxes(self):
        """
        Lists all available sandboxes.

        Returns:
            list: A list of dictionaries, each containing information about a sandbox.
        """
        response = await self._request("GET", "/sandboxes")
        return response.json()

    async def start_sandbox(self, sandbox_id):
        """Starts a specific sandbox."""
        await self._request("POST", f"/sandboxes/{sandbox_id}/start")

    async def exec_command(self, sandbox_id, command, standalone=False):
        """
        Executes a command in a specific sandbox.

        Args:
            sandbox_id (str): The ID of the sandbox to execute the command in.
            command (str): The command to execute.
            standalone (bool): Whether to execute the command in standalone mode.

        Returns:
            tuple[str, int]: The output and exit code from the sandbox.
        """
        payload = {"command": command, "standalone": bool(standalone)}
        path = f"/sandboxes/{sandbox_id}/exec"
        url = f"{self.server_url}{path}"
        async with httpx.AsyncClient(timeout=TIMEOUT) as client:
            response = await client.post(url, json=payload)
        if response.status_code >= 400:
            self._raise_api_error(response, path, context=f"command={command}")
        parsed = response.json()
        if (
            "output" not in parsed
            or "exit_code" not in parsed
            or "exited" not in parsed
        ):
            raise SoSAPIError(
                response.status_code, f"Missing fields in response: {parsed}", path
            )
        return parsed["output"], parsed["exit_code"], parsed["exited"]

    async def stop_sandbox(self, sandbox_id, remove=True):
        """
        Stops and removes a specific sandbox.

        Args:
            sandbox_id (str): The ID of the sandbox to stop.
            server_url (str): The base URL of the sandbox server.
        """
        await self._request(
            "POST", f"/sandboxes/{sandbox_id}/stop", json={"remove": remove}
        )

    async def get_sandbox_trajectory(self, sandbox_id, formatted=False):
        """
        Gets the trajectory of a specific sandbox.

        Args:
            sandbox_id (str): The ID of the sandbox to get the trajectory of.
            formatted (bool): Whether to format the trajectory as a list of commands.
        """
        response = await self._request(
            "GET",
            f"/sandboxes/{sandbox_id}/trajectory{'/formatted' if formatted else ''}",
        )
        trajectory = response.text if formatted else response.json()
        return trajectory


class SosParser(vf.Parser):
    def parse(self, text: str) -> Any:
        def parse_reasoning_from_response(response: str) -> Optional[str]:
            m = re.search(r"#Reasoning#:(.*)#Command#", response, re.DOTALL)
            return m.group(1).strip() if m else None

        def parse_command_from_response(response: str) -> Optional[str]:
            m = re.search(r"#Command#:(.*)$", response, re.DOTALL)
            return m.group(1).strip() if m else None

        reasoning = parse_reasoning_from_response(text)
        command = parse_command_from_response(text)
        return reasoning, command


class SoSEnv(vf.Environment):
    def __init__(
        self,
        default_image: str = "deathbyknowledge/shellm-sandbox:latest",
        sos_url: str = "http://localhost:3000",
        max_turns: int = 20,
        format: Literal["shellm", "chat"] = "shellm",
        **kwargs,
    ):
        self.format = format
        super().__init__(**kwargs)
        self.max_turns = max_turns
        self.sos = SoSClient(server_url=sos_url)
        self.default_image = default_image
        self.parser = vf.Parser() if format == "shellm" else SosParser()

    def format_dataset(
        self,
        dataset: Dataset,
        system_prompt: str | None = None,
        few_shot: list[vf.ChatMessage] | None = None,
        question_key: str = "task",  # Task prompt, used in place of "system" message
        answer_key: str = "answer",  # Success condition command, run to get reward (non-zero exit code)
    ) -> Dataset:
        assert "prompt" not in dataset.column_names, "prompt already exists"
        assert system_prompt is None, "system_prompt not expected"
        assert few_shot is None, "few_shot not expected"
        format = self.format

        # extract format_prompt as a standalone function to avoid capturing self
        def format_prompt_fn(task: str) -> list[vf.ChatMessage]:
            sys_template = f"""[SHELL MODE ENABLED]
User streams have been replaced with standard output and standard error streams.
Your outputs are not streamed to a user but piped directly to standard input.
To signal the completion of your task, use `exit 0`.
You must always validate that you successfully completed the task before exiting.
Do NOT attempt to solve the task in a single command. Run commands step by step. Piping and redirection are allowed.

Please reply strictly in the following format:
  #Reasoning#: [reasoning]
  #Command#: [command]

TASK: {task}
"""
            system_prompt = task if format == "shellm" else sys_template
            messages = []
            messages.append({"role": "system", "content": system_prompt})
            return messages

        return dataset.map(
            lambda x: {
                "prompt": format_prompt_fn(x[question_key]),
                "info": {
                    "setup_commands": x["setup_commands"],
                    "success_condition": x["success_condition"],
                },
            }
        )

    async def setup_state(self, state: vf.State, **kwargs) -> vf.State:
        state["exited"] = False
        state["exit_codes"] = []  # Used for reward calculation
        state["success"] = False
        return state
    
    async def cleanup(self, state: vf.State, **kwargs):
        if "sandbox_id" in state:
            await self.sos.stop_sandbox(state["sandbox_id"], remove=True)


    def is_completed(self, _: vf.Messages, state: vf.State, **kwargs) -> bool:
        return state["exited"]

    async def env_response(
        self, messages: vf.Messages, state: vf.State, **kwargs
    ) -> Tuple[vf.Messages, vf.State]:
        # return new environment message(s) + updated state
        assert isinstance(messages[-1], dict)
        assert "exited" in state
        assert "setup_commands" in state["info"]
        assert "success_condition" in state["info"]

        if "sandbox_id" not in state:
            try:
                sid = await self.sos.create_sandbox(
                    image=self.default_image, setup_commands=state["info"]["setup_commands"]
                )
                await self.sos.start_sandbox(sid)
                state["sandbox_id"] = sid
            except Exception as e:
                print(f"Error creating/starting sandbox: {e}")
                state["exited"] = True

        if self.format == "chat":
            response = messages[-1]["content"]
            reasoning, cmd = self.parser.parse(response)
            if not reasoning or not cmd:
                state["exited"] = True
                return [], state
            reasoning = [f"# {r}" for r in reasoning.split("\n")]
            reasoning = "\n".join(reasoning)
            cmd = f"{reasoning}\n{cmd}\n"
        else:
            cmd = messages[-1]["content"]
        sid = state["sandbox_id"]
        try:
            out, code, exited = await self.sos.exec_command(sid, cmd)
            state["exit_codes"].append(code)
            if exited:
                # must run success_condition and stop sandbox
                state["exited"] = True
                _, code, _ = await self.sos.exec_command(
                    sid, state["info"]["success_condition"], standalone=True
                )
                state["success"] = code == 0
            return [{"role": "user", "content": out}], state
        except Exception as e:
            print(f"Error executing command: {e}")
            state["exited"] = True
            return [], state

    async def rollout(
        self,
        client: AsyncOpenAI,
        model: str,
        prompt: vf.Messages,
        answer: str = "",
        task: str = "default",
        info: vf.Info | None = None,
        sampling_args: vf.SamplingArgs | None = None,
        **kwargs,
    ) -> tuple[vf.Messages, vf.State]:
        """
        Generate a multi-turn rollout with the environment (messages, state).
        """
        info = info or {}
        is_completed = False
        state = {
            "prompt": prompt,
            "completion": [],
            "answer": answer,
            "task": task,
            "info": info,
            "responses": [],
            "turn": 0,
        }
        state = await self.setup_state(state)
        if self.message_type == "chat":
            assert isinstance(prompt, list)
            completion = []
        else:
            assert isinstance(prompt, str)
            completion = ""
            state["responses_start_idx"] = []
        rollout = deepcopy(prompt)
        while not is_completed:
            if self.is_completed(rollout, state, **kwargs):
                is_completed = True
                break
            response = await self.get_model_response(
                client=client,
                model=model,
                prompt=rollout,
                oai_tools=info.get("oai_tools", None),
                sampling_args=sampling_args,
                message_type=self.message_type,
            )
            state["responses"].append(response)
            if self.message_type == "chat":
                assert isinstance(rollout, list)
                assert isinstance(completion, list)
                assert isinstance(response, vf.ChatCompletion)
                response_text: str = response.choices[0].message.content or ""  # type: ignore
                response_message: vf.ChatMessage = {
                    "role": "assistant",
                    "content": response_text,
                }
                if response.choices[0].message.tool_calls:
                    response_message["tool_calls"] = response.choices[  # type: ignore
                        0
                    ].message.tool_calls
                rollout.append(response_message)
                completion.append(response_message)
            else:
                assert isinstance(rollout, str)
                assert isinstance(completion, str)
                assert isinstance(response, vf.Completion)
                state["responses_start_idx"].append(len(completion))
                response_text: str = response.choices[0].text or ""  # type: ignore
                rollout += response_text
                completion += response_text
            state["turn"] += 1
            if (
                self.is_completed(rollout, state, **kwargs)
                or state["turn"] >= self.max_turns
            ):
                is_completed = True
            else:
                env_msgs, state = await self.env_response(rollout, state, **kwargs)
                if self.message_type == "chat":
                    assert isinstance(env_msgs, list)
                    assert isinstance(rollout, list)
                    assert isinstance(completion, list)
                    rollout += env_msgs
                    completion += env_msgs
                else:
                    assert isinstance(env_msgs, str)
                    assert isinstance(rollout, str)
                    assert isinstance(completion, str)
                    rollout += env_msgs
                    completion += env_msgs
        await self.cleanup(state, **kwargs)
        return completion, state


def load_environment(**kwargs) -> vf.Environment:
    dataset: Dataset = load_dataset("deathbyknowledge/shell-tasks", split="train")
    eval_dataset: Dataset = load_dataset("deathbyknowledge/shell-tasks", split="test")
    parser = vf.Parser()

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

    def check_correctness(parser, completion, state, **kwargs) -> float:
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
            R = success_term - exit_penalty - fail_penalty

            return max(-1.0, min(1.0, R))

        assert isinstance(completion, list)
        num_turns = len(
            [m["content"] for m in parser.get_assistant_messages(completion)]
        )
        nonzero_exits = sum(1 for c in state["exit_codes"] if c != 0)
        return reward_from_outcome(state["success"], num_turns, nonzero_exits)

    rubric = vf.Rubric(parser=parser, funcs=[check_correctness])

    vf_env = SoSEnv(
        dataset=dataset,
        eval_dataset=eval_dataset,
        parser=parser,
        rubric=rubric,
        **kwargs,
    )

    return vf_env
