# On the cost function

The idea is a wallet shouldn't broadcast the instant you ask.
It holds a queue of user intents and calculates the least costly way to execute them.

An intent is a desired user outcome. It may be realized in multiple ways. e.g getting paid, spending a coin, creating an output.

Costs are expressed in satoshis and composed of two parts:

* Objective costs: the amount of value that will leave the wallet.
* Subjective costs: cost of anything else the user may care about. i.e timeliness, privacy, etc.

Some subjective costs may be privacy related for example:

* Revealing new utxos to previous receiver.
* Anonymity set of a coin
* non-derivved sub tx mapping (or an estimate of it)

Another example is timeliness. i.e when a wallet needs to satify an intent.

Some intents may be realized only unilaterally. This may be the user preference or just arbitrary.
Othertimes an intent may have to be expressed unilaterally bc we are up against time pressure.

If a intent's deadline dominates cost function should prefer to batch those togheter (if its not a privacy liability) and broadcast soon.
While if we are ok with waiting, we should prefer to wait for a better opportunity (i.e peers come online or I am expecting ot get paid by an interactive capable peer)

Actions are general commital steps towards satisfying an intent. e.g initiating an interactive session, creating a unilateral transaction.

The leaves of these decision tree are concrete txs. In the interactive settings the leaf may be converted to a interactive Plan.

====

the spectrum of choices that can be made in the interactive settings are optimizing for two bounds:

* block space cost
* Privacy cost

On one hand we have a strategy that optimizes for block space cost - one big batch, i.e one sub tx which coins were seleted for
One the other hand we have a strategy that optimizes for privacy cost - multiple sub txs, i.e each coin is used in a different sub tx.

The middle ground is what we need to calculate. Calcluating this is a combinatorial problem -- O(bellnumber(n)) where n is the number of intents. or O(fubini(n)) where n is the number of intents where weak ordering matters.

What we are permuating on is the sets of intents in each sub tx / batch. Each sub tx will change decompose / select coins independently.

Importantly, the block space cost and privacy cost of the middle ground is bounded by each side of the spectrum. i.e someting in the middle cannot cost less in terms of either cost than the best strategy on each side.

We start to group high importantce intents togehter in sub txs / batches but we do not enumurate over all possibilities. Bc the tacit assumption is that the cost of one subtx over another is likely negligible.

And once new infomration is obtained from the other counterparties we will recalculate parts of the the middle ground.

Furthermore, if we know when a term in the cost function will perform teribly somewhere in the combinatorial space we stop early.
For example, if we a privacy term says we cannot (cost is infinite) afford to link 3 inputs together we never enumerate over the space of 4 inputs.

==== Notes

Greedy paritions refinement. Some intents may be inbalanced. We have some target fee rate.
We do coin selection for the upper range of the fee rate on the batches of intents
Things should be balanced or imbalanced (change is needed) -- specific change output has not been specified.
For each change option there is a sub-tree for different ways to decompose the change.

Input choices are dependent between paritions of intents in a batch. Have to use mutaully exclusive coins for different batches.

The input that is selected between a bunch of partitions are the ones that are commits to the most leaves so they should be registed first.
