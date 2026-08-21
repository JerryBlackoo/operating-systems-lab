"""Training loop (card notes/cs336-a1/19-training-together.md).

Example (tiny CPU smoke run on synthetic data):
    python -m cs336_basics.train --train-data /tmp/train.npy --val-data /tmp/val.npy \
        --vocab-size 64 --context-length 32 --d-model 64 --num-heads 4 --d-ff 128 \
        --num-layers 2 --batch-size 8 --total-iters 100 --device cpu
"""

from __future__ import annotations

import argparse
import time

import numpy as np
import torch

from .checkpoint import load_checkpoint, save_checkpoint
from .data import get_batch
from .model import TransformerLM
from .optim import AdamW, cross_entropy, gradient_clipping, lr_cosine_schedule


def parse_args(argv=None):
    p = argparse.ArgumentParser(description="Train a Transformer LM from scratch")
    # Data
    p.add_argument("--train-data", required=True, help=".npy file of uint16 token ids")
    p.add_argument("--val-data", required=True)
    # Model
    p.add_argument("--vocab-size", type=int, required=True)
    p.add_argument("--context-length", type=int, default=256)
    p.add_argument("--d-model", type=int, default=512)
    p.add_argument("--num-heads", type=int, default=16)
    p.add_argument("--d-ff", type=int, default=1344)
    p.add_argument("--num-layers", type=int, default=4)
    p.add_argument("--rope-theta", type=float, default=10000.0)
    # Optimizer / schedule
    p.add_argument("--lr-max", type=float, default=3e-4)
    p.add_argument("--lr-min", type=float, default=3e-5)
    p.add_argument("--warmup-iters", type=int, default=100)
    p.add_argument("--beta1", type=float, default=0.9)
    p.add_argument("--beta2", type=float, default=0.95)
    p.add_argument("--eps", type=float, default=1e-8)
    p.add_argument("--weight-decay", type=float, default=0.01)
    p.add_argument("--grad-clip", type=float, default=1.0)
    # Loop
    p.add_argument("--batch-size", type=int, default=32)
    p.add_argument("--total-iters", type=int, default=5000)
    p.add_argument("--eval-interval", type=int, default=100)
    p.add_argument("--eval-batches", type=int, default=10)
    p.add_argument("--log-interval", type=int, default=20)
    p.add_argument("--ckpt-path", default=None)
    p.add_argument("--ckpt-interval", type=int, default=1000)
    p.add_argument("--resume", default=None)
    p.add_argument("--device", default="cpu")
    p.add_argument("--wandb-project", default=None)
    return p.parse_args(argv)


@torch.no_grad()
def evaluate(model, data, batch_size, context_length, device, n_batches) -> float:
    model.eval()
    losses = []
    for _ in range(n_batches):
        inputs, targets = get_batch(data, batch_size, context_length, device)
        losses.append(cross_entropy(model(inputs), targets).item())
    model.train()
    return float(np.mean(losses))


def main(argv=None):
    args = parse_args(argv)
    # Memory-mapped loading: never read the whole dataset into RAM.
    train_data = np.load(args.train_data, mmap_mode="r")
    val_data = np.load(args.val_data, mmap_mode="r")

    model = TransformerLM(
        vocab_size=args.vocab_size,
        context_length=args.context_length,
        num_layers=args.num_layers,
        d_model=args.d_model,
        num_heads=args.num_heads,
        d_ff=args.d_ff,
        rope_theta=args.rope_theta,
        device=args.device,
    )
    optimizer = AdamW(
        model.parameters(),
        lr=args.lr_max,
        betas=(args.beta1, args.beta2),
        eps=args.eps,
        weight_decay=args.weight_decay,
    )

    start_iter = 0
    if args.resume:
        start_iter = load_checkpoint(args.resume, model, optimizer)
        print(f"resumed from {args.resume} at iteration {start_iter}")

    wandb_run = None
    if args.wandb_project:
        import wandb

        wandb_run = wandb.init(project=args.wandb_project, config=vars(args))

    model.train()
    t0 = time.time()
    for it in range(start_iter, args.total_iters):
        inputs, targets = get_batch(train_data, args.batch_size, args.context_length, args.device)
        loss = cross_entropy(model(inputs), targets)
        optimizer.zero_grad()
        loss.backward()
        grad_norm = gradient_clipping(model.parameters(), args.grad_clip)
        lr_t = lr_cosine_schedule(it, args.lr_max, args.lr_min, args.warmup_iters, args.total_iters)
        for group in optimizer.param_groups:
            group["lr"] = lr_t
        optimizer.step()

        if it % args.log_interval == 0:
            wall = time.time() - t0
            print(
                f"iter {it:6d} | loss {loss.item():.4f} | lr {lr_t:.2e} "
                f"| grad_norm {grad_norm:.2f} | wall {wall:7.1f}s"
            )
            if wandb_run:
                wandb_run.log(
                    {"train/loss": loss.item(), "lr": lr_t, "grad_norm": grad_norm}, step=it
                )
        if args.eval_interval and it > 0 and it % args.eval_interval == 0:
            val_loss = evaluate(
                model, val_data, args.batch_size, args.context_length, args.device, args.eval_batches
            )
            print(f"iter {it:6d} | val_loss {val_loss:.4f}")
            if wandb_run:
                wandb_run.log({"val/loss": val_loss}, step=it)
        if args.ckpt_path and it > 0 and it % args.ckpt_interval == 0:
            save_checkpoint(model, optimizer, it, args.ckpt_path)

    if args.ckpt_path:
        save_checkpoint(model, optimizer, args.total_iters, args.ckpt_path)
        print(f"saved final checkpoint to {args.ckpt_path}")
    final_val = evaluate(
        model, val_data, args.batch_size, args.context_length, args.device, args.eval_batches
    )
    print(f"final val_loss {final_val:.4f}")
    return final_val


if __name__ == "__main__":
    main()
