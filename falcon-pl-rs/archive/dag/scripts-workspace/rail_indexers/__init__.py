"""Chain indexers for the rule-bound Falcon rail agent.

Indexers only *observe* external chains and emit Falcon-shaped header/proof
payloads. They never mint; Falcon protocol `can_apply` is the authority.
"""

from .factory import make_indexer
from .base import ChainIndexer, HeaderBatch, LockDeposit

__all__ = ["make_indexer", "ChainIndexer", "HeaderBatch", "LockDeposit"]
