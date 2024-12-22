import subprocess
import os

def run_gdb(commands):
    # Start gdb process
    gdb_process = subprocess.Popen(['gdb', '-q', '--interpreter=mi3'], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)

    # Send commands to gdb
    for command in commands:
        gdb_process.stdin.write(command + '\n')
        gdb_process.stdin.flush()

    # Send quit command to gdb
    gdb_process.stdin.write('quit\n')
    gdb_process.stdin.flush()

    # Get output and errors
    output, errors = gdb_process.communicate()

    return output, errors

def parse_gdb_output(output):
    # Simple parser to extract useful information from gdb output
    parsed_output = []
    for line in output.splitlines():
        if line.startswith('(gdb)'):
            continue
        parsed_output.append(line)
    return parsed_output

if __name__ == "__main__":
    # Get the absolute path of the application binary
    binary_path = os.path.abspath(os.path.join(os.path.dirname(__file__), '../apps/raft/build/app/node'))
    commands = [
        f'file {binary_path}',
        'break main',
        'run',
        'info registers',
        'backtrace'
    ]

    output, errors = run_gdb(commands)
    if errors:
        print("Errors:", errors)
    else:
        parsed_output = parse_gdb_output(output)
        for line in parsed_output:
            print(line)