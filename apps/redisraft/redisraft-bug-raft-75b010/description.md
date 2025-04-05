# Bug Description

## Source: 
https://github.com/RedisLabs/raft/commit/75b01044440bb5587948232cae2fd76e0d311265

## Description: 

In a network partition situation where we have two leaders inside the partitioned cluster. When the old leader rejoins the cluster, if it sends out the appendentries to the others first and receives the responses before the new leader sends the appendentries request, a bug is triggered. The old leader cannot correctly step down when see itself is an outdated leader.

If the new leader sends the appendentries first and it is handled by the old leader before anything else, the bug is masked.