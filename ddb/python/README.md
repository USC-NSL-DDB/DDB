# Interactive Distributed Debugger (DDB)

## Usage

``` bash
ddb configs/dbg_auto_discovery.yaml
```

A configuration file is required. Simple example:

``` yaml
---
SSH:
  user: ybyan # you can skip this if the username used to connect is same as the current user

ServiceDiscovery:
  Broker:
    hostname: 10.10.1.2 # the communication IP of starting machine that can be reached from other machines
```