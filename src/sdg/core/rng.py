"""Named random streams derived from one seed.

Each stage draws from its own stream, keyed by name, so changing how many numbers one
stage draws (a new alarm knob, a bigger history) leaves every other stage's output
byte-identical. One shared generator would reshuffle everything downstream of the change,
and a diff of two runs would show the whole dataset moving when one knob did.

Stream keys come from sha256 of the name, not Python's `hash()`, which is salted per
process and would make "the same seed" mean different data on every run.
"""

from __future__ import annotations

import hashlib

import numpy as np


class Streams:
    def __init__(self, seed: int):
        self.seed = int(seed)
        self._cache: dict[str, np.random.Generator] = {}

    def __call__(self, name: str) -> np.random.Generator:
        if name not in self._cache:
            key = int.from_bytes(hashlib.sha256(name.encode()).digest()[:8], "big")
            self._cache[name] = np.random.default_rng(np.random.SeedSequence([self.seed, key]))
        return self._cache[name]
