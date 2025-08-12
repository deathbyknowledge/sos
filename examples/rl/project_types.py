from typing import List, Optional, Literal
from pydantic import BaseModel, Field

class Message(BaseModel):
  role: Literal['assistant', 'user', 'system']
  content: str

class Scenario(BaseModel):
  id: str
  task: str
  setup_commands: List[str]
  success_condition: str


class RunConfig(BaseModel):
  num_epochs: int = 2
  groups_per_step: int = 12
  validation_frequency: int = 10
  validation_num_scenarios: int = 20
  training_num_scenarios: int = 1000
  rollouts_per_group: int = 8
  learning_rate: float = 1e-5
  difficulty: Optional[Literal[1, 2, 3, 4]] = None
