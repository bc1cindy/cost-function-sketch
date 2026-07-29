# Fungi Cost Function Sketch

This document sketches a (unfinished) privacy-aware, generalized coin selection algorithm for the [Fungi protocol](https://github.com/fungi-protocol/docs).

## Overview

Undermining wallet clustering requires wallets to batch interactively with other participants. This repo sketches out a model which helps wallets decide when and how to batch with other participants.

Breifly, users push intents onto a queue. An intent encodes a desired outcome. e.g paying a peer under a set of payment instructions, spending a specific coin, or receiving a payment at some future point.

Each intent carries a deadline and a payoff, expressed together as a piecewise linear utility function. Utility decays as the deadline approaches. When an intent falls due and carries no privacy sensitivity, the wallet may mark it unilateral and broadcast it alone. Otherwise, the wallet realizes the intent inside a batch of other intents in an interactive transaction.

## What are we optimizing for?

The algorithm optimizes along two axes:

* Weight: the blockspace the transaction consumes.
* Privacy: the resistance of the transaction to clustering heuristics, decomposable into several metrics.

The two axes are inversely related. e.g privacy costs blockspace.

Take a set of three intents: `a`, `b`, and `c`. The most aggressive batching strategy merges all three, selects coins once, and decomposes change once. Depending on how peers contribute during the protocol, the resulting sub-transaction may stand out, which links its inputs and outputs. In exchange, this strategy consumes the least blockspace.

The opposite extreme selects mutually exclusive coins and decomposes change separately for each intent. This consumes more blockspace but may resist linkage more effectively.

The optimum sits between these extremes. Treat the extremes as bounds on the two terms of the optimization problem, blockspace and privacy. (Time enters the problem as well TODO.)

<!--  the per-intent strategy forms the greatest lower bound, the fully merged strategy the least upper bound. The problem reduces to lattice refinement. -->

## Small Intent Counts

For small `N`, enumerate every partition of the intent set and select the partition that minimizes the cost function. The leaf count of the decision tree equals the nth Bell number.

## The Pipeline

Each leaf of the decision tree represents a set of intents together with the coins selected to fund them. Each leaf also roots a sub-tree of candidate change decomposition strategies.

Read a leaf as a proposed state transition on the wallet. Simulate the transition, then score it with the objective (amount leaving the wallet) and subjective cost (deadline and privacy metrics) terms described above.

Once scoring identifies the lowest-cost leaf, that branch represents the plan. The plan  is a state transition that "good enough" even if no peer contributes meaningfully (interms of privacy) to the collaborative transaction. As peers come online and supply new information, the wallet revises the plan. A branch that yields a sparse transaction in isolation may score better once a peer reveals an input.

## Large Intent Counts

As a reminder, the two extremes mentioned above bound the search space. Meaning, no leaf costs less than the best strategy on either side, so on finding a leaf that costs more than a bound, we can stop early. As enumerating the remainder of that branch cannot reduce the cost further. This is analogous to stopping early in a branch and bound algorithms.

Lastly, we can assume negligible cost between groupings. The cost gap between `{a,b}, {c}` and `{a,c}, {b}` is usually negligible. A greedy strategy therefore groups the "highest-importance" intents first and skips full enumeration. Additionally, if we know some of the terms of the cost function we can stop early. e.g if we know we cannot afford to link 3 inputs together we never enumerate over the space of 4 inputs.

Note that large intent counts are not yet supported and this part is the least worked out.

Things to do and questions to answer:

* [ ] Introduce privacy metrics. Which ones do we start with?
* [ ] Pay offs are not being considered yet.
* [ ] Mutually exclusive coins for each batch
