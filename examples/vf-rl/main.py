import os
from openai import AsyncOpenAI
import verifiers as vf
from verifiers.types import (
    Messages,
    State,
)
from rich import print

vf_env = vf.load_environment("vf-sos", sos_url="http://localhost:3000", max_turns=20)

outputs = vf_env.evaluate(
    client=AsyncOpenAI(api_key=os.getenv("API_KEY", "MISSING_KEY"), base_url=os.getenv("BASE_URL", "http://rearden:8000/v1")),
    model="deathbyknowledge/Qwen2.5-7B-Shell-SFT",
    sampling_args=vf.SamplingArgs(temperature=0.0),
    num_examples=10,
    rollouts_per_example=1,
)


print(outputs)