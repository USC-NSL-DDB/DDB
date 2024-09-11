import * as ChildProcess from "child_process";

// Spawn the child process
const child = ChildProcess.spawn("/home/hjz/.pyenv/versions/3.10.5/bin/python", ["pyplay.py"]);
// Function to handle stdout
function stdout(data) {
    console.log(data.toString());
}

// Listen to stdout data
child.stdout.on("data", stdout);
child.stderr.on("data", stdout );
// Listen for SIGINT signal on the parent process
process.on("SIGINT", () => {
    console.log("Received SIGINT. Sending SIGINT to child process...");
    child.kill('SIGINT'); // Send SIGINT to the child process
});

// Listen for the child process to exit
child.on("exit", (code, signal) => {
    console.log(`Child process exited with code ${code} and signal ${signal}`);
    process.exit(); // Exit the parent process
});
setTimeout(()=>{
    child.stdin.write("-exec-run\n")
    for(let i=0;i<2000;i++){
        let token=10000+i;
        child.stdin.write(`${token}-thread-info\n`);
    }
},6000);
setInterval(()=>{
    child.stdin.write("\n")
},1000);
// Keep the Node.js process running until you hit Ctrl+C
console.log("Node.js process is running. Press Ctrl+C to exit.");
setInterval(() => {}, 1000); // Keep the event loop busy
