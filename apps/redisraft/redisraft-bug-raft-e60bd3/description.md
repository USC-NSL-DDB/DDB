# Bug Description

## Source: 
https://github.com/RedisLabs/raft/commit/e60bd38079a91ddbd8df14e884b170574652855a

## Description: 
The leader is erroneously forced to step down when receives the recv_requestvote.

## Replication Steps:

1. Start and initialize the entire cluster.
2. Set a bkpt at `raft_periodic_internal` for "node3" and wait for bkpt to hit.
3. Step the process that hits the breakpoint until the election timeout checkpoint.
4. Modify the `me->election_timeout_rand` so that it triggers the timeout immediately. Another alternative is to use "Jump to cursor" to directly jump to the `raft_election_start` line.
5. Keep the "node3" paused and figure out who is the current leader.
6. Pause the current leader process and let the remaining one running.
7. The remaining one will keep increasing the term to try to start new elections.
8. Re-join the old leader. After this, both nodes should reconcile with each other and elect a new leader.
9. When it is stable, figure out the new leader and set a bkpt at `raft_recv_requestvote` on the new leader.
10. Rejoin the "node3" by continuing it.
11. The new leader should hit the bkpt at `raft_recv_requestvote` and now step over to examine the flow.
