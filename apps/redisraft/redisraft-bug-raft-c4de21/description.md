# Bug Description

## Source:
https://github.com/RedisLabs/raft/commit/c4de21ed19371afe30b32686d9a9f45450c350a3

## Description:

When the leader sends out append_entries and process the corresponding responses, the leader will update the commit index. In this bug, during the process of commit index update, the leader check the quorum based on the total number of nodes, instead of just voting numbers. Therefore, it can cause a live-lock and the commit index fails to advance.

## Changelog:
`raft_server.c`: `raft_update_commit_idx`


## Function flow of adding a node into the cluster
1. the joining node receives the join command from a client and runs `redisraft.c` -> `cmdRaftCluster`.
2. the node sends a RAFT NODE ADD request to the leader via `join.c` -> `JoinCluster`.
3. the leader receives the request in `redisraft.c` -> `cmdRaftNode`.
4. the leader prepares and appends the log entry to the logs in `redisraft.c` -> `cmdRaftNode`.
5. the entry appending happens in `common.c` -> `RedisRaftRecvEntry` and `raft_server.c` -> `raft_recv_entry`.
6. when the entry is committed, it runs `raft.c` -> `raftApplyLog`, which replies to the async node add request from the joining node.
7. the joining node receives the response and executes the handler in `join.c` -> `handleNodeAddResponse`.
8. inside this function, if everything goes ok, it will run the callback `redisraft.c` -> `clusterJoinCompleted`, which prepares the logs.
