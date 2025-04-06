# Bug Description

## Source:
https://github.com/RedisLabs/raft/commit/c4de21ed19371afe30b32686d9a9f45450c350a3

## Description:

When the leader sends out append_entries and process the corresponding responses, the leader will update the commit index. In this bug, during the process of commit index update, the leader check the quorum based on the total number of nodes, instead of just voting numbers. Therefore, it can cause a live-lock and the commit index fails to advance.

## Changelog:
`raft_server.c`: `raft_update_commit_idx`
