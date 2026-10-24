# Roadmap

Intentions only. Nothing in this file is being built now.

Testnet 2300 is not promoted into mainnet. Mainnet is a new network with its own genesis. Until that ships, Falcon runs the three things a new node cannot supply for itself: who may join, the only full copy of history, and the public door the installer dials. Block production is already the bonded seats. The lottery is equal. A larger bond does not buy more turns.

## Now

Lite nodes only. The public installer starts an observer or, after admission and a 50,000 FPL bond, a lite validator that keeps 128 ledgers. Falcon keeps the archive (v1), the hub (v3), the admission key, and the seven seed ports. A bonded home node dials those ports and is not added to them.

## Mainnet, still one operator at genesis

Genesis names the first seats. The admission public key is baked into that release. Scott approves the next seats with the admission key. That key cannot move funds or seal blocks. Losing it only stops new seats. History is still one archive until a second operator keeps a full copy. The installer lists Falcon's seed first.

## When outsiders are actually sealing

These are the conditions for stepping back, not a date.

- Enough outside seats are bonded that six remain if Falcon's seats unbond.
- At least two archives, run by different operators, are advertised and can serve a join snapshot. A false archive flag can be revoked by the admission key. The floor already pays an archive 105 instead of 100 on the 10% floor.
- The installer lists more than one public door, run by more than one operator. Falcon's relay may stay as one line. People can add peers by hand.

Falcon's own seats can then unbond. Keeping an archive or one seed after that is optional. Treasury emission does not depend on those seats being online.

## Not in this phase

Replacing the single admission key with more than one approver. That is a later release, because the public key is compiled into the binary. Also later, and tracked in `docs/ops/consensus-lottery-rewards.md` §4.3: a committee larger than six, quorum that scales with that committee, gossip that is not a full mesh, and a state root that hashes only the changes.
