from time import sleep
from pygdbmi.gdbcontroller import GdbController
from pprint import pprint
from gdb_manager import GdbManager
from yaml import load, dump, safe_load, YAMLError

import sys
import argparse

ARGS = [
    ["gdb", "./nu_bin/test_migrate", "-l", "1", "-i", "18.18.1.3"],
    ["gdb", "./nu_bin/test_migrate", "-l", "1", "-i", "18.18.1.4"],
    ["gdb", "./nu_bin/test_migrate", "-l", "1", "-i", "18.18.1.5", "-m"],
]

def eprint(*args, **kwargs):
    print(*args, file=sys.stderr, **kwargs)

def print_resp(response):
    if response["stream"] == "stdout":
        print(f"[{response['type']}] {response['payload']}", end="")
    if response["stream"] == "stderr":
        eprint(f"[{response['type']}] {response['payload']}", end="")

def main():
    components = config_data["Components"]
    # pprint(components)
    gdb_manager = GdbManager(components=components)

    # while True:
    #     sleep(1)

    # del gdb_manager
    # gdbmi = GdbController(["gdb", "./bin/hello_world", "--interpreter=mi"])
    # print(gdbmi.command)  # print actual command run as subprocess
    # for response in gdbmi.get_gdb_response():
    #     print_resp(response)
    #     pprint(response)
        
    while True:
        cmd = input("(gdb) ").strip()
        gdb_manager.write(cmd)
        # cmd_head = cmd.split()[0]

        # if cmd_head in ["break", "b", "-break-insert"]:
        #     # gdbmi.write
        #     gdb_manager.write(cmd)

        # else:
        #     responses = gdbmi.write(cmd)
        #     for response in responses:
        #         print_resp(response)
        #         pprint(response)
        
    # gdbmi.exit()

if __name__ == "__main__":
    parser = argparse.ArgumentParser(
        description="interactive debugging for distributed software.",
    )

    parser.add_argument(
        "config", 
        metavar="conf_file", 
        type=str,
        help="Path of the debugging config file."
    )

    args = parser.parse_args()

    config_data = None

    with open(str(args.config), "r") as fs:
        try:
            config_data = safe_load(fs)
            print("Loaded dbg config file:")
            pprint(config_data)
        except YAMLError as e:
            eprint(f"Failed to read the debugging config. Error: {e}")

    if not config_data:
        eprint("Debugging config is required!")
        exit(1)
            
    main()
