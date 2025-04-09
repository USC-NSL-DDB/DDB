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

## Debugging Procedure with DDB

1. start the cluster but do not initialize any server yet.
2. run `redis-cli -p 5001 raft.cluster init` to initialize the first server.
3. we want to investigate why the joining node hangs.
4. Before joining the new node, set some bkpts at 
   1. `redisraft.c:1215` @ joining node, where the joining node receives the join command from the client,
   2. `redisraft.c:108` @ leader node, where the leader node receives the node add command from the joining node,
   3. `raft.c:1027` @ leader node, where the leader node apply the committed log (a.k.a. finishing joining the node),
   4. `join.c:25` @ joining node, where the joining node receives the response from the leader indicating the joining operation is finished. After this, the joining node will response back to the client.
5. Then, run command `redis-cli -p 5002 RAFT.CLUSTER JOIN localhost:5001` to join the new node to the cluster.
6. DDB stops at the 1st bkpt, and we examine if anything goes wrong at this stage.
7. Continue, and DDB stops at the 2nd bkpt, and we step over to see if the entry is successfully appended to the log.
8. Continue, and DDB never stops at the 3rd or 4th bkpts.
9. We make the conclusion that the entry is never considered committed or applied.
10. We need to add more bkpts to understand the issue.
11. Set a bkpt at `raft_server.c:2257` (`raft_flush`), where the raft periodically commit the entries.
12. Continue, and DDB stops at the new bkpt. 
13. Step over/in and examine carefully to see which behavior looks abnormal.
14. Inside `raft_server.c:raft_update_commit_idx`, when steps to the line 2233, we saw the `commit` is less than `me->commit_idx`.
15. Then, check the logic of how the `commit` is calculated. At this time, we check the `indexes` and found that there are two values and the match index at the 2nd element is 0.
16. This is the issue! Since the joining node starts in the "non-voting` mode, therefore, it didn't participate in the process of log replication. Therefore, the leader will never see it advances its match index, which remains 0.
