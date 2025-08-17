import argparse

import verifiers as vf

"""
# install
vf-install vf-sos (-p /path/to/environments)

# quick eval
vf-eval vf-wordle -m (model_name in endpoints.py)

7b inference:
NCCL_P2P_DISABLE=1 NCCL_IB_DISABLE=1 NCCL_SHM_DISABLE=1 CUDA_VISIBLE_DEVICES=0 vf-vllm --model deathbyknowledge/Qwen2.5-7B-Shell-SFT --enforce-eager --disable-log-requests

7b training:
NCCL_P2P_DISABLE=1 NCCL_IB_DISABLE=1 NCCL_SHM_DISABLE=1 CUDA_VISIBLE_DEVICES=1 accelerate launch --num-processes 1 --config-file zero3.yaml train.py --size 7B
"""


def main(args):
    size = args.size
    model_name = f"deathbyknowledge/Qwen2.5-{size}-Shell-SFT"
    model, tokenizer = vf.get_model_and_tokenizer(model_name)
    vf_env = vf.load_environment(env_id="vf-sos", max_turns=20)
    run_name = f"vf-sos-grpo-{size}"
    training_args = vf.grpo_defaults(run_name=run_name)
    training_args.per_device_train_batch_size = 4 
    training_args.num_generations = 8 
    training_args.gradient_accumulation_steps = 2 
    training_args.max_tokens = 1024 # per turn
    training_args.max_seq_len = 8192
    training_args.max_steps = 500 
    training_args.eval_strategy = "steps"
    training_args.eval_steps = 25
    training_args.mask_env_responses = True
    training_args.max_grad_norm = 0.1 
    training_args.beta = 0.0 

    trainer = vf.GRPOTrainer(
        model=model,
        processing_class=tokenizer,
        env=vf_env,
        args=training_args,
        # lora_config=vf.lora_defaults()
    )   
    trainer.train()


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--size", "-s", type=str, default="7B")
    args = parser.parse_args()
    main(args)