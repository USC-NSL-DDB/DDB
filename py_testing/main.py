from pygdbmi.gdbcontroller import GdbController
from pprint import pprint

import sys

ARGS = [
    [""]
]

def eprint(*args, **kwargs):
    print(*args, file=sys.stderr, **kwargs)

def print_resp(response):
    if response["stream"] == "stdout":
        print(f"[{response['type']}] {response['payload']}", end="")
    if response["stream"] == "stderr":
        eprint(f"[{response['type']}] {response['payload']}", end="")

def main():
    gdbmi = GdbController(["gdb", "./bin/hello_world", "--interpreter=mi"])
    print(gdbmi.command)  # print actual command run as subprocess
    for response in gdbmi.get_gdb_response():
        print_resp(response)
        pprint(response)
        
    while True:
        cmd = input("(gdb) ")
        responses = gdbmi.write(cmd)
        for response in responses:
            print_resp(response)
            pprint(response)
        
    gdbmi.exit()

if __name__ == "__main__":
    main()
