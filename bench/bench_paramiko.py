import paramiko
import time

def benchmark_paramiko(hostname, port, username, key_filename, command, iterations=10):
    client = paramiko.SSHClient()
    client.set_missing_host_key_policy(paramiko.AutoAddPolicy())
    client.load_system_host_keys()

    try:
        client.connect(hostname, port=port, username=username, key_filename=key_filename)
        
        total_time = 0
        for i in range(iterations):
            start_time = time.time()
            stdin, stdout, stderr = client.exec_command(command)
            stdout.channel.recv_exit_status()  # Wait for command to finish
            end_time = time.time()
            
            total_time += (end_time - start_time)
            print(f"Iteration {i+1}: {end_time - start_time:.4f} seconds")
        
        average_time = total_time / iterations
        print(f"Average execution time over {iterations} iterations: {average_time:.4f} seconds")
    
    finally:
        client.close()

if __name__ == "__main__":
    hostname = "localhost"
    port = 22
    username = "ybyan"
    key_filename = "/home/ybyan/.ssh/id_rsa.pub"  # Replace with the path to your private key
    command = "cat /dev/null | head 1000000"
    
    benchmark_paramiko(
        hostname, port, username, key_filename, command,
        iterations=10000
    )
