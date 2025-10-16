import { Breakpoint, IBackend, Thread, Stack, SSHArguments, Variable, RegisterValue, VariableObject, MIError, SingleBreakpoint } from "../backend";
import * as ChildProcess from "child_process";
import { EventEmitter } from "events";
import { parseMI, MINode } from '../mi_parse';
import * as linuxTerm from '../linux/console';
import * as net from "net";
import * as fs from "fs";
import * as path from "path";
const axios = require('axios');
export function escape(str: string) {
	return str.replace(/\\/g, "\\\\").replace(/"/g, "\\\"");
}

const nonOutput = /^(?:\d*|undefined)[\*\+\=]|[\~\@\&\^]/;
const gdbMatch = /(?:\d*|undefined)\(gdb\)/;
const numRegex = /\d+/;

function couldBeOutput(line: string) {
	if (nonOutput.exec(line))
		return false;
	return true;
}

const trace = process.env.TRACE?.toLowerCase() === 'true';
function pollServiceUntilReady(endpoint, maxAttempts = 30, interval = 1000): Promise<void> {
	return new Promise((resolve, reject) => {
		let attempts = 0;

		const checkStatus = () => {
			axios.get(endpoint)
				.then(response => {
					if (response.data.status === "up") {
						console.log("Service is ready!");
						resolve();
					} else {
						attempts++;
						if (attempts >= maxAttempts) {
							reject(new Error("Max attempts reached. Service not ready."));
						} else {
							setTimeout(checkStatus, interval);
						}
					}
				})
				.catch(error => {
					attempts++;
					if (attempts >= maxAttempts) {
						reject(error);
					} else {
						setTimeout(checkStatus, interval);
					}
				});
		};

		checkStatus();
	});
}
class LogMessage {
	protected logMsgVar = "";
	protected logMsgVarProcess = "";
	protected logMsgRplNum = 0;
	protected logMsgRplItem: string[] = [];
	protected logMsgMatch = /(^\$[0-9]*[\ ]*=[\ ]*)(.*)/;
	protected logReplaceTest = /{([^}]*)}/g;
	public logMsgBrkList: Breakpoint[] = [];

	logMsgOutput(record: any) {
		if ((record.type === 'console')) {
			if (record.content.startsWith("$")) {
				const content = record.content;
				const variableMatch = this.logMsgMatch.exec(content);
				if (variableMatch) {
					const value = content.substr(variableMatch[1].length).trim();
					this.logMsgRplItem.push(value);
					this.logMsgRplNum--;
					if (this.logMsgRplNum == 0) {
						for (let i = 0; i < this.logMsgRplItem.length; i++) {
							this.logMsgVarProcess = this.logMsgVarProcess.replace("placeHolderForVariable", this.logMsgRplItem[i]);
						}
						return "Log Message:" + this.logMsgVarProcess;
					}
				}
			}
			return undefined;
		}
	}

	logMsgProcess(parsed: MINode) {
		this.logMsgBrkList.forEach((brk) => {
			if (parsed.outOfBandRecord[0].output[0][1] == "breakpoint-hit" && parsed.outOfBandRecord[0].output[2][1] == brk.id) {
				this.logMsgVar = brk?.logMessage;
				const matches = this.logMsgVar.match(this.logReplaceTest);
				const count = matches ? matches.length : 0;
				this.logMsgRplNum = count;
				this.logMsgVarProcess = this.logMsgVar.replace(this.logReplaceTest, "placeHolderForVariable");
				this.logMsgRplItem = [];
			}
		});
	}
}

export class MI2 extends EventEmitter implements IBackend {
	constructor(public application: string, public preargs: string[], public extraargs: string[], procEnv: any, public extraCommands: string[] = []) {
		super();
		this.vscodeFrameToDDBFrame = new ThreadFrameMapper();
		if (procEnv) {
			const env: { [key: string]: string } = {};
			// Duplicate process.env so we don't override it
			for (const key in process.env)
				if (process.env.hasOwnProperty(key))
					env[key] = process.env[key];

			// Overwrite with user specified variables
			for (const key in procEnv) {
				if (procEnv.hasOwnProperty(key)) {
					if (procEnv === undefined)
						delete env[key];
					else
						env[key] = procEnv[key];
				}
			}
			this.procEnv = env;
		}
	}
	protected logMessage: LogMessage = new LogMessage;

	load(cwd: string, target: string, procArgs: string, separateConsole: string, autorun: string[]): Thenable<any> {
		// if (!path.isAbsolute(target))
		// 	target = path.join(cwd, target);
		return new Promise((resolve, reject) => {
			this.isSSH = false;
			const args = this.preargs.concat(this.extraargs || []);
			this.process = ChildProcess.spawn(this.application, args
				// { cwd: cwd, env: this.procEnv }
			);
			setInterval(() => (this.process.stdin.write("\n")), 2000)
			this.process.stdout.on("data", this.stdout.bind(this));
			this.process.stderr.on("data", this.stderr.bind(this));
			this.process.on("exit", () => this.emit("quit"));
			this.process.on("error", err => this.emit("launcherror", err));
			// const promises = this.initCommands(target, cwd);
			const promises = []
			const apiBaseUrl = process.env.DDB_API_URL || 'http://localhost:5000';
			promises.push(pollServiceUntilReady(`${apiBaseUrl}/status`));
			Promise.all(promises).then(() => {
				this.emit("debug-ready");
				resolve(undefined);
			}, reject);
			// 	if (procArgs && procArgs.length)
			// 		promises.push(this.sendCommand("exec-arguments " + procArgs));
			// 	if (process.platform == "win32") {
			// 		if (separateConsole !== undefined)
			// 			promises.push(this.sendCommand("gdb-set new-console on"));
			// 		promises.push(...autorun.map(value => { return this.sendUserInput(value); }));
			// 		Promise.all(promises).then(() => {
			// 			this.emit("debug-ready");
			// 			resolve(undefined);
			// 		}, reject);
			// 	} else {
			// 		if (separateConsole !== undefined) {
			// 			linuxTerm.spawnTerminalEmulator(separateConsole).then(tty => {
			// 				promises.push(this.sendCommand("inferior-tty-set " + tty));
			// 				promises.push(...autorun.map(value => { return this.sendUserInput(value); }));
			// 				Promise.all(promises).then(() => {
			// 					this.emit("debug-ready");
			// 					resolve(undefined);
			// 				}, reject);
			// 			});
			// 		} else {
			// 			promises.push(...autorun.map(value => { return this.sendUserInput(value); }));
			// 			Promise.all(promises).then(() => {
			// 				this.emit("debug-ready");
			// 				resolve(undefined);
			// 			}, reject);
			// 		}
			// 	}
		});
	}

	ssh(args: SSHArguments, cwd: string, target: string, procArgs: string, separateConsole: string, attach: boolean, autorun: string[]): Thenable<any> {
		return new Promise((resolve, reject) => {
		});
	}

	protected initCommands(target: string, cwd: string, attach: boolean = false) {
		// We need to account for the possibility of the path type used by the debugger being different
		// from the path type where the extension is running (e.g., SSH from Linux to Windows machine).
		// Since the CWD is expected to be an absolute path in the debugger's environment, we can test
		// that to determine the path type used by the debugger and use the result of that test to
		// select the correct API to check whether the target path is an absolute path.
		// const debuggerPath = path.posix.isAbsolute(cwd) ? path.posix : path.win32;

		// if (!debuggerPath.isAbsolute(target))
		// 	target = debuggerPath.join(cwd, target);

		const cmds = [
			// this.sendCommand("gdb-set target-async on", true),
			// new Promise(resolve => {
			// 	this.sendCommand("list-features").then(done => {
			// 		this.features = done.result("features");
			// 		resolve(undefined);
			// 	}, () => {
			// 		// Default to no supported features on error
			// 		this.features = [];
			// 		resolve(undefined);
			// 	});
			// }),
			// this.sendCommand("environment-directory \"" + escape(cwd) + "\"", true)
		];
		// if (!attach)
		// 	cmds.push(this.sendCommand("file-exec-and-symbols \"" + escape(target) + "\""));
		if (this.prettyPrint)
			cmds.push(this.sendCommand("enable-pretty-printing"));
		for (const cmd of this.extraCommands) {
			cmds.push(this.sendCommand(cmd));
		}
		return cmds;
	}

	attach(cwd: string, executable: string, target: string, autorun: string[]): Thenable<any> {
		return new Promise((resolve, reject) => {
			let args = [];
			if (executable && !path.isAbsolute(executable))
				executable = path.join(cwd, executable);
			args = this.preargs.concat(this.extraargs || []);
			this.process = ChildProcess.spawn(this.application, args, { cwd: cwd, env: this.procEnv });
			this.process.stdout.on("data", this.stdout.bind(this));
			this.process.stderr.on("data", this.stderr.bind(this));
			this.process.on("exit", () => this.emit("quit"));
			this.process.on("error", err => this.emit("launcherror", err));
			const promises = this.initCommands(target, cwd, true);
			if (target.startsWith("extended-remote")) {
				promises.push(this.sendCommand("target-select " + target));
				if (executable)
					promises.push(this.sendCommand("file-symbol-file \"" + escape(executable) + "\""));
			} else {
				// Attach to local process
				if (executable)
					promises.push(this.sendCommand("file-exec-and-symbols \"" + escape(executable) + "\""));
				promises.push(this.sendCommand("target-attach " + target));
			}
			promises.push(...autorun.map(value => { return this.sendUserInput(value); }));
			Promise.all(promises).then(() => {
				this.emit("debug-ready");
				resolve(undefined);
			}, reject);
		});
	}

	connect(cwd: string, executable: string, target: string, autorun: string[]): Thenable<any> {
		return new Promise((resolve, reject) => {
			let args = [];
			if (executable && !path.isAbsolute(executable))
				executable = path.join(cwd, executable);
			args = this.preargs.concat(this.extraargs || []);
			if (executable)
				args = args.concat([executable]);
			this.process = ChildProcess.spawn(this.application, args, { cwd: cwd, env: this.procEnv });
			this.process.stdout.on("data", this.stdout.bind(this));
			this.process.stderr.on("data", this.stderr.bind(this));
			this.process.on("exit", () => this.emit("quit"));
			this.process.on("error", err => this.emit("launcherror", err));
			const promises = this.initCommands(target, cwd, true);
			promises.push(this.sendCommand("target-select remote " + target));
			promises.push(...autorun.map(value => { return this.sendUserInput(value); }));
			Promise.all(promises).then(() => {
				this.emit("debug-ready");
				resolve(undefined);
			}, reject);
		});
	}

	stdout(data) {
		if (typeof data == "string")
			this.buffer += data;
		else
			this.buffer += data.toString("utf8");
		const end = this.buffer.lastIndexOf('\n');
		if (end != -1) {
			this.onOutput(this.buffer.substring(0, end));
			this.buffer = this.buffer.substring(end + 1);
		}
	}

	stderr(data: any) {
		if (typeof data == "string")
			this.errbuf += data;
		else
			this.errbuf += data.toString("utf8");
		const end = this.errbuf.lastIndexOf('\n');
		if (end != -1) {
			// this.onOutputStderr(this.errbuf.substring(0, end));
			this.errbuf = this.errbuf.substring(end + 1);
		}
		if (this.errbuf.length) {
			// this.logNoNewLine("stderr", this.errbuf);
			this.errbuf = "";
		}
	}

	onOutputStderr(str: string) {
		const lines = str.split('\n');
		lines.forEach(line => {
			this.log("stderr", line);
		});
	}

	onOutputPartial(line: string) {
		if (couldBeOutput(line)) {
			this.logNoNewLine("stdout", line);
			return true;
		}
		return false;
	}
	extractMIOutput(input) {
		const pattern = /\[ TOOL MI OUTPUT \] \\n(.*)\\n/s;
		const match = input.match(pattern);

		if (match && match[1]) {
			return match[1];
		} else {
			return null; // Return null if no match is found or the capture group is empty
		}
	}
	onOutput(str: string) {
		const lines = str.split('\n');
		let miOutputStarted = false;
		lines.forEach(line => {
			// console.log("raw Line:", line);
			// if (couldBeOutput(line)) {
			// 	// if (!gdbMatch.exec(line))
			// 	// 	this.log("stdout", line);
			// } else {
			// if (line.trim() == "(ddb)") {
			// 	miOutputStarted = true;
			// } else if (miOutputStarted) {
			console.log("parsing line:", ` ${line}`);
			const parsed = parseMI(line);
			if (this.debugOutput)
				this.log("log", "GDB -> App: " + JSON.stringify(parsed));
			let handled = false;
			if (parsed.token !== undefined) {
				if (this.handlers[parsed.token]) {
					this.handlers[parsed.token](parsed);
					delete this.handlers[parsed.token];
					handled = true;
				}
			}
			if (!handled && parsed.resultRecords && parsed.resultRecords.resultClass == "error") {
				this.log("stderr", parsed.result("msg") || line);
			}
			if (parsed.outOfBandRecord.length > 0) {
				parsed.outOfBandRecord.forEach(record => {
					if (record.isStream) {
						this.log(record.type, record.content);
						const logOutput = this.logMessage.logMsgOutput(record);
						if (logOutput) {
							this.log("console", logOutput);
						}
					} else {
						if (record.type == "exec") {
							this.emit("exec-async-output", parsed);
							if (record.asyncClass == "running")
								this.emit("running", record);
							else if (record.asyncClass == "stopped") {
								const reason = parsed.record("reason");
								if (reason === undefined) {
									if (trace)
										this.log("stderr", "stop (no reason given)");
									// attaching to a process stops, but does not provide a reason
									// also python generated interrupt seems to only produce this
									this.emit("step-other", parsed);
								} else {
									if (trace)
										this.log("stderr", "stop: " + reason);
									switch (reason) {
										case "breakpoint-hit":
											this.emit("breakpoint", parsed);
											this.logMessage.logMsgProcess(parsed);
											break;
										case "watchpoint-trigger":
										case "read-watchpoint-trigger":
										case "access-watchpoint-trigger":
											this.emit("watchpoint", parsed);
											break;
										case "function-finished":
										// identical result → send step-end
										// this.emit("step-out-end", parsed);
										// break;
										case "location-reached":
										case "end-stepping-range":
											this.emit("step-end", parsed);
											break;
										case "watchpoint-scope":
										case "solib-event":
										case "syscall-entry":
										case "syscall-return":
											// TODO: inform the user
											this.emit("step-end", parsed);
											break;
										case "fork":
										case "vfork":
										case "exec":
											// TODO: inform the user, possibly add second inferior
											this.emit("step-end", parsed);
											break;
										case "signal-received":
											this.emit("signal-stop", parsed);
											break;
										// case "exited-normally":
										// 	this.emit("exited-normally", parsed);
										// 	break;
										// case "exited": // exit with error code != 0
										// 	this.log("stderr", "Program exited with code " + parsed.record("exit-code"));
										// 	this.emit("exited-normally", parsed);
										// 	break;
										// case "exited-signalled":	// consider handling that explicit possible
										// 	this.log("stderr", "Program exited because of signal " + parsed.record("signal"));
										// 	this.emit("stopped", parsed);
										// 	break;

										default:
											this.log("console", "Not implemented stop reason (assuming exception): " + reason);
											this.emit("stopped", parsed);
											break;
									}
								}
							} else
								this.log("log", JSON.stringify(parsed));
						} else if (record.type == "notify") {
							if (record.asyncClass == "thread-created") {
								this.emit("thread-created", parsed);
							} else if (record.asyncClass == "thread-exited") {
								this.emit("thread-exited", parsed);
							}
						}
					}
				});
				handled = true;
			}
			if (parsed.token == undefined && parsed.resultRecords == undefined && parsed.outOfBandRecord.length == 0)
				handled = true;
			if (!handled)
				this.log("log", "Unhandled: " + JSON.stringify(parsed));
			miOutputStarted = false
			// }
		});
	}

	start(runToStart: boolean): Thenable<boolean> {
		const options: string[] = [];
		if (runToStart)
			options.push("--start");
		const startCommand: string = ["exec-run"].concat(options).join(" ");
		return new Promise((resolve, reject) => {
			this.log("console", "Running executable");
			this.sendCommand(startCommand).then((info) => {
				if (info.resultRecords.resultClass == "running")
					resolve(undefined);
				else
					reject();
			}, reject);
		});
	}

	async stop() {
		// if (this.isSSH) {
		// 	const proc = this.stream;
		// 	const to = setTimeout(() => {
		// 		proc.signal("KILL");
		// 	}, 1000);
		// 	this.stream.on("exit", function (code) {
		// 		clearTimeout(to);
		// 	});
		// 	this.sendRaw("-gdb-exit");
		// } else {
		return new Promise((resolve) => {
			const proc = this.process;
			proc.kill('SIGINT');

			this.process.on("exit", (code) => {
				resolve(code);
			});
		});
		// this.sendRaw("-gdb-exit");
		// }
	}

	detach() {
		const proc = this.process;
		const to = setTimeout(() => {
			process.kill(-proc.pid);
		}, 1000);
		this.process.on("exit", function (code) {
			clearTimeout(to);
		});
		this.sendRaw("-target-detach");
	}

	interrupt(): Thenable<boolean> {
		if (trace)
			this.log("stderr", "interrupt");
		return new Promise((resolve, reject) => {
			this.sendCommand("exec-interrupt").then((info) => {
				resolve(info.resultRecords.resultClass == "done");
			}, reject);
		});
	}

	continue(reverse: boolean = false): Thenable<boolean> {
		if (trace)
			this.log("stderr", "continue");
		return new Promise((resolve, reject) => {
			if (trace)
				this.log("stderr", `continuehandle continueRequest`);
			this.sendCommand("record-time-and-continue" + (reverse ? " --reverse" : "")).then((info) => {
				resolve(info.resultRecords.resultClass == "running");
			}, reject);
			// this.sendCommand("exec-continue" + (reverse ? " --reverse" : "")).then((info) => {
			// 	resolve(info.resultRecords.resultClass == "running");
			// }, reject);
		});
	}
	switchThread(thread: number): Thenable<boolean> {
		if (trace)
			this.log("stderr", `switch thread to${thread}`);
		return new Promise((resolve, reject) => {
			this.sendCommand(`thread-select ${thread}`).then((info) => {
				resolve(info.resultRecords.resultClass == "done");
			}, reject);
		});
	}

	next(thread: number, reverse: boolean = false): Thenable<boolean> {
		if (trace)
			this.log("stderr", "next");
		return new Promise((resolve, reject) => {
			this.sendCommand("record-time-and-next" + ` --thread ${thread}` + (reverse ? " --reverse" : "")).then((info) => {
				resolve(info.resultRecords.resultClass == "running");
			}, reject);
			// this.sendCommand("exec-next" + ` --thread ${thread}` + (reverse ? " --reverse" : "")).then((info) => {
			// 	resolve(info.resultRecords.resultClass == "running");
			// }, reject);
		});
	}

	step(thread: number, reverse: boolean = false): Thenable<boolean> {
		if (trace)
			this.log("stderr", "step");
		return new Promise((resolve, reject) => {
			this.sendCommand("record-time-and-step" + ` --thread ${thread}` + (reverse ? " --reverse" : "")).then((info) => {
				resolve(info.resultRecords.resultClass == "running");
			}, reject);
			// this.sendCommand("exec-step" + ` --thread ${thread}` + (reverse ? " --reverse" : "")).then((info) => {
			// 	resolve(info.resultRecords.resultClass == "running");
			// }, reject);
		});
	}

	stepOut(thread: number, reverse: boolean = false): Thenable<boolean> {
		if (trace)
			this.log("stderr", "stepOut");
		return new Promise((resolve, reject) => {
			this.sendCommand("record-time-and-finish" + ` --thread ${thread}` + (reverse ? " --reverse" : "")).then((info) => {
				resolve(info.resultRecords.resultClass == "running");
			}, reject);
			// this.sendCommand("exec-finish" + ` --thread ${thread}` + (reverse ? " --reverse" : "")).then((info) => {
			// 	resolve(info.resultRecords.resultClass == "running");
			// }, reject);
		});
	}

	goto(filename: string, line: number, sessionId: number): Thenable<Boolean> {
		if (trace)
			this.log("stderr", "goto");
		return new Promise((resolve, reject) => {
			const target: string = '"' + (filename ? escape(filename) + ":" : "") + line + '"';
			this.sendCommand("break-insert -t -f " + target + ` --session ${sessionId}`).then(() => {
				this.sendCommand("exec-jump " + target + ` --session ${sessionId}`).then((info) => {
					resolve(info.resultRecords.resultClass == "running");
				}, reject);
			}, reject);
		});
	}

	changeVariable(name: string, rawValue: string): Thenable<any> {
		if (trace)
			this.log("stderr", "changeVariable");
		return this.sendCommand("gdb-set var " + name + "=" + rawValue);
	}

	loadBreakPoints(breakpoints: Breakpoint[]): Thenable<[boolean, Breakpoint][]> {
		if (trace)
			this.log("stderr", "loadBreakPoints");
		console.log("loadBreakPoints", breakpoints)
		const promisses: Thenable<any>[] = [];
		breakpoints.forEach(breakpoint => {
			promisses.push(this.addBreakPoint(breakpoint));
		});
		return Promise.all(promisses);
	}

	setBreakPointCondition(bkptNum: number, condition: string, sessionId: string): Thenable<any> {
		if (trace)
			this.log("stderr", "setBreakPointCondition");
		return this.sendCommand("break-condition " + bkptNum + " " + condition + " --session " + sessionId);
	}

	setLogPoint(bkptNum: number, command: string, sessionId: string): Thenable<any> {
		const regex = /{([a-z0-9A-Z-_\.\>\&\*\[\]]*)}/gm;
		let m: RegExpExecArray;
		let commands: string = "";

		while ((m = regex.exec(command))) {
			if (m.index === regex.lastIndex) {
				regex.lastIndex++;
			}
			if (m[1]) {
				commands += `\"print ${m[1]}\" `;
			}
		}
		return this.sendCommand("break-commands " + bkptNum + " " + commands + " --session " + sessionId);
	}

	setEntryBreakPoint(entryPoint: string): Thenable<any> {
		return this.sendCommand("break-insert -t -f " + entryPoint);
	}

	// addBreakPoint(breakpoint: Breakpoint): Thenable<[boolean, Breakpoint]> {
	// 	if (trace)
	// 		this.log("stderr", "addBreakPoint");
	// 	return new Promise((resolve, reject) => {
	// 		const locations=[]
	// 		if (this.breakpoints.has(breakpoint))
	// 			return resolve([false, undefined]);
	// 		let location = "";
	// 		if (breakpoint.countCondition) {
	// 			if (breakpoint.countCondition[0] == ">")
	// 				location += "-i " + numRegex.exec(breakpoint.countCondition.substring(1))[0] + " ";
	// 			else {
	// 				const match = numRegex.exec(breakpoint.countCondition)[0];
	// 				if (match.length != breakpoint.countCondition.length) {
	// 					this.log("stderr", "Unsupported break count expression: '" + breakpoint.countCondition + "'. Only supports 'X' for breaking once after X times or '>X' for ignoring the first X breaks");
	// 					location += "-t ";
	// 				} else if (parseInt(match) != 0)
	// 					location += "-t -i " + parseInt(match) + " ";
	// 			}
	// 		}
	// 		if (breakpoint.raw)
	// 			location += '"' + escape(breakpoint.raw) + '"';
	// 		else
	// 			location += '"' + escape(breakpoint.file) + ":" + breakpoint.line + '"';
	// 		if(breakpoint.allSessions){
	// 			locations.push(location + "--all");
	// 		}else if(breakpoint.sessionIds){
	// 			breakpoint.sessionIds.forEach((sessionId)=>{
	// 				locations.push(location + " --session " + sessionId);
	// 			}
	// 			);
	// 		}
	// 		const promises = locations.map((locations)=>{})
	// 		this.sendCommand("break-insert -f " + location).then((result) => {
	// 			if (result.resultRecords.resultClass == "done") {
	// 				const bkptNum = parseInt(result.result("bkpt.number"));
	// 				const newBrk = {
	// 					id: bkptNum,
	// 					file: breakpoint.file ? breakpoint.file : result.result("bkpt.file"),
	// 					raw: breakpoint.raw,
	// 					line: parseInt(result.result("bkpt.line")),
	// 					condition: breakpoint.condition,
	// 					logMessage: breakpoint?.logMessage,
	// 				};
	// 				if (breakpoint.condition) {
	// 					this.setBreakPointCondition(bkptNum, breakpoint.condition).then((result) => {
	// 						if (result.resultRecords.resultClass == "done") {
	// 							this.breakpoints.set(newBrk, bkptNum);
	// 							resolve([true, newBrk]);
	// 						} else {
	// 							resolve([false, undefined]);
	// 						}
	// 					}, reject);
	// 				} else if (breakpoint.logMessage) {
	// 					this.setLogPoint(bkptNum, breakpoint.logMessage).then((result) => {
	// 						if (result.resultRecords.resultClass == "done") {
	// 							breakpoint.id = newBrk.id;
	// 							this.breakpoints.set(newBrk, bkptNum);
	// 							this.logMessage.logMsgBrkList.push(breakpoint);
	// 							resolve([true, newBrk]);
	// 						} else {
	// 							resolve([false, undefined]);
	// 						}
	// 					}, reject);
	// 				} else {
	// 					this.breakpoints.set(newBrk, bkptNum);
	// 					resolve([true, newBrk]);
	// 				}
	// 			} else {
	// 				reject(result);
	// 			}
	// 		}, reject);
	// 	});
	// }

	async addSingleBreakPoint(breakpoint: SingleBreakpoint): Promise<Boolean> {
		if (trace)
			this.log("stderr", "addSingleBreakPoint");

		let location = "";
		if (breakpoint.countCondition) {
			if (breakpoint.countCondition[0] === ">") {
				const count = numRegex.exec(breakpoint.countCondition.substring(1))?.[0];
				if (count) {
					location += "-i " + count + " ";
				} else {
					this.log("stderr", "Invalid count condition format.");
				}
			} else {
				const match = numRegex.exec(breakpoint.countCondition)?.[0];
				if (match) {
					if (match.length !== breakpoint.countCondition.length) {
						this.log("stderr", "Unsupported break count expression: '" + breakpoint.countCondition + "'. Only supports 'X' for breaking once after X times or '>X' for ignoring the first X breaks");
						location += "-t ";
					} else if (parseInt(match, 10) !== 0) {
						location += "-t -i " + parseInt(match, 10) + " ";
					}
				} else {
					this.log("stderr", "Invalid count condition format.");
				}
			}
		}

		if (breakpoint.raw) {
			location += '"' + escape(breakpoint.raw) + '"';
		} else {
			location += '"' + escape(breakpoint.file) + ":" + breakpoint.line + '"';
		}


		// Map each location to a promise that sets the breakpoint
		const sessionId = breakpoint.sessionId;
		const bkptPathLineId = this.generateBreakpointId(breakpoint.file, breakpoint.line);
		if (this.breakpoints.get(bkptPathLineId)?.has(sessionId)) {
			return true
		}
		try {
			const result = await this.sendCommand("break-insert -f " + location + " --session " + sessionId);
			if (result.resultRecords.resultClass === "done") {
				const bkptNum = parseInt(result.result("bkpt.number"), 10);
				const newBrk: SingleBreakpoint = {
					id: bkptNum,
					file: breakpoint.file ? breakpoint.file : result.result("bkpt.file"),
					raw: breakpoint.raw,
					line: breakpoint.line,
					condition: breakpoint.condition,
					logMessage: breakpoint.logMessage,
					verified: true,
					sessionId: sessionId
				};
				if (!this.breakpoints.has(bkptPathLineId)) {
					this.breakpoints.set(bkptPathLineId, new Map());
				}

				// Handle condition
				if (breakpoint.condition) {
					const condResult = await this.setBreakPointCondition(bkptNum, breakpoint.condition, sessionId);
					if (condResult.resultRecords.resultClass !== "done") {
						throw new Error(`Failed to set condition for breakpoint ${bkptNum}`);
					}
				}

				// Handle log message
				if (breakpoint.logMessage) {
					const logResult = await this.setLogPoint(bkptNum, breakpoint.logMessage, sessionId);
					if (logResult.resultRecords.resultClass !== "done") {
						throw new Error(`Failed to set log message for breakpoint ${bkptNum}`);
					}
				}

				this.breakpoints.get(bkptPathLineId).set(sessionId, newBrk);
				return true;
			} else {
				throw new Error(`Failed to insert breakpoint for location: ${location} and session: ${sessionId}`);
			}
		} catch (error) {
			return false;
		}
	}
	async addBreakPoint(breakpoint: Breakpoint): Promise<Breakpoint> {
		if (trace)
			this.log("stderr", "addBreakPoint");

		let location = "";
		if (breakpoint.countCondition) {
			if (breakpoint.countCondition[0] === ">") {
				const count = numRegex.exec(breakpoint.countCondition.substring(1))?.[0];
				if (count) {
					location += "-i " + count + " ";
				} else {
					this.log("stderr", "Invalid count condition format.");
				}
			} else {
				const match = numRegex.exec(breakpoint.countCondition)?.[0];
				if (match) {
					if (match.length !== breakpoint.countCondition.length) {
						this.log("stderr", "Unsupported break count expression: '" + breakpoint.countCondition + "'. Only supports 'X' for breaking once after X times or '>X' for ignoring the first X breaks");
						location += "-t ";
					} else if (parseInt(match, 10) !== 0) {
						location += "-t -i " + parseInt(match, 10) + " ";
					}
				} else {
					this.log("stderr", "Invalid count condition format.");
				}
			}
		}

		if (breakpoint.raw) {
			location += '"' + escape(breakpoint.raw) + '"';
		} else {
			location += '"' + escape(breakpoint.file) + ":" + breakpoint.line + '"';
		}


		// Map each location to a promise that sets the breakpoint
		const promises = breakpoint.sessionIds.map((sessionId) => {
			return async () => {
				const bkptPathLineId = this.generateBreakpointId(breakpoint.file, breakpoint.line);
				if (this.breakpoints.get(bkptPathLineId)?.has(sessionId)) {
					return { success: true, sessionId, newBrk: this.breakpoints.get(bkptPathLineId).get(sessionId) };
				}
				try {
					const result = await this.sendCommand("break-insert -f " + location + " --session " + sessionId);
					if (result.resultRecords.resultClass === "done") {
						const bkptNum = parseInt(result.result("bkpt.number"), 10);
						const newBrk: SingleBreakpoint = {
							id: bkptNum,
							file: breakpoint.file ? breakpoint.file : result.result("bkpt.file"),
							raw: breakpoint.raw,
							line: parseInt(result.result("bkpt.line"), 10),
							condition: breakpoint.condition,
							logMessage: breakpoint.logMessage,
							verified: true,
							sessionId: sessionId
						};
						if (!this.breakpoints.has(bkptPathLineId)) {
							this.breakpoints.set(newBrk.file, new Map());
						}

						// Handle condition
						if (breakpoint.condition) {
							const condResult = await this.setBreakPointCondition(bkptNum, breakpoint.condition, sessionId);
							if (condResult.resultRecords.resultClass !== "done") {
								throw new Error(`Failed to set condition for breakpoint ${bkptNum}`);
							}
						}

						// Handle log message
						if (breakpoint.logMessage) {
							const logResult = await this.setLogPoint(bkptNum, breakpoint.logMessage, sessionId);
							if (logResult.resultRecords.resultClass !== "done") {
								throw new Error(`Failed to set log message for breakpoint ${bkptNum}`);
							}
						}

						this.breakpoints.get(bkptPathLineId).set(sessionId, newBrk);
						return { success: true, sessionId, newBrk };
					} else {
						throw new Error(`Failed to insert breakpoint for location: ${location} and session: ${sessionId}`);
					}
				} catch (error) {
					return { success: false, sessionId, error: error.message };
				}
			}
		});
		const results = await Promise.all(promises.map(promiseFunc => promiseFunc()));
		const successfulResults = results.filter(result => result.success);

		breakpoint.sessionIds = successfulResults.map(result => result.sessionId);
		return breakpoint;
	}





	async removeSingleBreakPoint(breakpoint: SingleBreakpoint): Promise<boolean> {
		if (trace) {
			this.log("stderr", "removeBreakPoint");
		}

		if (!this.breakpoints.has(this.generateBreakpointId(breakpoint.file, breakpoint.line))) {
			return false;
		}
		return this.sendCommand("break-delete " + breakpoint.id + " --session " + breakpoint.sessionId).then((result) => {
			if (result.resultRecords.resultClass === "done") {
				this.breakpoints.get(this.generateBreakpointId(breakpoint.file, breakpoint.line))?.delete(breakpoint.sessionId);
				const bkpts = this.breakpoints.get(this.generateBreakpointId(breakpoint.file, breakpoint.line));
				if (bkpts && bkpts.size === 0) {
					this.breakpoints.delete(this.generateBreakpointId(breakpoint.file, breakpoint.line));
				}
				return true;
			} else {
				return false;
			}
		})
	}
	async removeBreakPoint(breakpoint: Breakpoint): Promise<boolean> {
		if (trace) {
			this.log("stderr", "removeBreakPoint");
		}

		const promises = []
		if (!this.breakpoints.has(this.generateBreakpointId(breakpoint.file, breakpoint.line))) {
			return false;
		}
		this.breakpoints.get(this.generateBreakpointId(breakpoint.file, breakpoint.line)).forEach((bkpt, _) => {
			promises.push(this.sendCommand("break-delete " + bkpt.id + " --session " + bkpt.sessionId).then((result) => {
				if (result.resultRecords.resultClass === "done") {
					this.breakpoints.delete(this.generateBreakpointId(breakpoint.file, breakpoint.line));
					return true;
				} else {
					return false;
				}
			}));
		});
		const results = await Promise.all(promises);
		return results.some((result) => result === true);
	}
	generateBreakpointId(file: string, line: number): string {
		return `${file}|||${line}`;
	}
	getLineFromBreakpointId(id: string): number {
		return parseInt(id.split("|||")[1]);
	}
	getFileFromBreakpointId(id: string): string {
		return id.split("|||")[0];
	}
	clearBreakPoints(source?: string): Thenable<any> {
		if (trace)
			this.log("stderr", "clearBreakPoints");
		const promises = []
		this.breakpoints.forEach((bkpts, pathId) => {
			if (pathId.startsWith(source)) {
				bkpts.forEach((bkpt, _) => {
					promises.push(this.sendCommand("break-delete " + bkpt.id + " --session " + bkpt.sessionId).then((result) => {
						if (result.resultRecords.resultClass == "done") {
							this.breakpoints.delete(pathId);
							return true;
						} else {
							return false;
						}
					}));
				});
			}
		});
		return Promise.all(promises)
	}

	async getThreads(): Promise<Thread[]> {
		if (trace) this.log("stderr", "getThreads");

		const command = "thread-info";
		const result = await this.sendCommand(command);
		const threads = result.result("threads");
		const ret: Thread[] = [];
		if (!Array.isArray(threads)) { // workaround for lldb-mi bug: `'^done,threads="[]"'`
			return ret;
		}
		return threads.map(element => {
			const ret: Thread = {
				id: parseInt(MINode.valueOf(element, "id")),
				targetId: MINode.valueOf(element, "target-id"),
				name: MINode.valueOf(element, "name") || MINode.valueOf(element, "details")
			};

			return ret;
		});
	}

	async getStack(startFrame: number, maxLevels: number, thread: number): Promise<Stack[]> {
		if (trace) this.log("stderr", "getStack");

		// const options: string[] = [];

		// if (thread != 0)
		// 	options.push("--thread " + thread);

		// const depth: number = (await this.sendCommand(["stack-info-depth"].concat(options).join(" "))).result("depth").valueOf();
		// const lowFrame: number = startFrame ? startFrame : 0;
		// const highFrame: number = (maxLevels ? Math.min(depth, lowFrame + maxLevels) : depth) - 1;

		// if (highFrame < lowFrame)
		// 	return [];

		// options.push(lowFrame.toString());
		// options.push(highFrame.toString());

		// const result = await this.sendCommand(["stack-list-frames"].concat(options).join(" "));
		const result = await this.sendCommand(`bt-remote --thread ${thread}`);
		const stack = result.result("stack");

		return stack.map((element: any) => {
			const level = MINode.valueOf(element, "level");
			const addr = MINode.valueOf(element, "addr");
			const func = MINode.valueOf(element, "func");
			const filename = MINode.valueOf(element, "file");
			const session_id = MINode.valueOf(element, "session");
			const thread_id = MINode.valueOf(element, "thread");
			let file: string = MINode.valueOf(element, "fullname");
			if (!file) {
				// Fallback to using `file` if `fullname` is not provided.
				// GDB does this for some reason when frame filters are used.
				file = filename
			}
			if (file) {
				if (this.isSSH)
					file = path.posix.normalize(file);
				else
					file = path.normalize(file);
			}

			let line = 0;
			const lnstr = MINode.valueOf(element, "line");
			if (lnstr)
				line = parseInt(lnstr);
			const from = parseInt(MINode.valueOf(element, "from"));

			// vscode
			return {
				address: addr,
				fileName: filename,
				file: file,
				function: func || from,
				level: level,
				line: line,
				session: session_id,
				thread: thread_id
			};
		});
	}
	// @ts-ignore
	async getStackVariables(thread: number, frame: number, session: number): Promise<Variable[]> {
		if (trace)
			this.log("stderr", "getStackVariables");

		const result = await this.sendCommand(`stack-list-variables --thread ${thread} --frame ${frame} --all-values`);
		// const result = await this.sendCommand(`stack-list-variables --thread ${thread} --frame 0 --simple-values --session ${session}`);
		const variables = result.result("variables");
		const ret: Variable[] = [];
		for (const element of variables) {
			const key = MINode.valueOf(element, "name");
			const value = MINode.valueOf(element, "value");
			const type = MINode.valueOf(element, "type");
			if (!key) {
				// Skip if any of the required properties are missing
				continue;
			}
			ret.push({
				name: key,
				valueStr: value,
				type: type,
				raw: element
			});
		}
		return ret;
	}

	async getRegisters(): Promise<Variable[]> {
		if (trace)
			this.log("stderr", "getRegisters");

		// Getting register names and values are separate GDB commands.
		// We first retrieve the register names and then the values.
		// The register names should never change, so we could cache and reuse them,
		// but for now we just retrieve them every time to keep it simple.
		const names = await this.getRegisterNames();
		const values = await this.getRegisterValues();
		const ret: Variable[] = [];
		for (const val of values) {
			const key = names[val.index];
			const value = val.value;
			const type = "string";
			ret.push({
				name: key,
				valueStr: value,
				type: type
			});
		}
		return ret;
	}

	async getRegisterNames(): Promise<string[]> {
		if (trace)
			this.log("stderr", "getRegisterNames");
		const result = await this.sendCommand("data-list-register-names");
		const names = result.result('register-names');
		if (!Array.isArray(names)) {
			throw new Error('Failed to retrieve register names.');
		}
		return names.map(name => name.toString());
	}

	async getRegisterValues(): Promise<RegisterValue[]> {
		if (trace)
			this.log("stderr", "getRegisterValues");
		const result = await this.sendCommand("data-list-register-values N");
		const nodes = result.result('register-values');
		if (!Array.isArray(nodes)) {
			throw new Error('Failed to retrieve register values.');
		}
		const ret: RegisterValue[] = nodes.map(node => {
			const index = parseInt(MINode.valueOf(node, "number"));
			const value = MINode.valueOf(node, "value");
			return { index: index, value: value };
		});
		return ret;
	}

	examineMemory(from: number, length: number): Thenable<any> {
		if (trace)
			this.log("stderr", "examineMemory");
		return new Promise((resolve, reject) => {
			this.sendCommand("data-read-memory-bytes 0x" + from.toString(16) + " " + length).then((result) => {
				resolve(result.result("memory[0].contents"));
			}, reject);
		});
	}
	//@ts-ignore
	async evalExpression(name: string, thread: number, frame: number, session: number): Promise<MINode> {
		if (trace)
			this.log("stderr", "evalExpression");

		let command = "data-evaluate-expression ";
		if (thread != 0) {
			command += `--thread ${thread} --frame ${frame} `;
		}
		command += name;

		return await this.sendCommand(command);
	}

	async varCreate(threadId: number, frameLevel: number, expression: string, name: string = "-", frame: string = "@"): Promise<VariableObject> {
		if (trace)
			this.log("stderr", "varCreate");
		let miCommand = "var-create ";
		if (threadId != 0) {
			miCommand += `--thread ${threadId} --frame ${frameLevel}`;
		}
		const res = await this.sendCommand(`${miCommand} ${this.quote(name)} ${frame} "${expression}"`);
		return new VariableObject(res.result(""), threadId);
	}

	async varEvalExpression(name: string): Promise<MINode> {
		if (trace)
			this.log("stderr", "varEvalExpression");
		return this.sendCommand(`var-evaluate-expression ${this.quote(name)}`);
	}

	async varListChildren(threadId: number, name: string, parent?: VariableObject): Promise<VariableObject[]> {
		if (trace)
			this.log("stderr", "varListChildren");
		//TODO: add `from` and `to` arguments
		let miCommand = "var-list-children "
		if (threadId != 0) {
			miCommand += `--thread ${threadId}`;
		}
		const res = await this.sendCommand(`${miCommand} --all-values ${this.quote(name)}`);
		const children = res.result("children") || [];
		const omg: VariableObject[] = children.map((child: any) => new VariableObject(child, threadId, parent));
		return omg;
	}

	async varUpdate(threadId: number, frameLevel: number, name: string = "*"): Promise<MINode> {
		if (trace)
			this.log("stderr", "varUpdate");
		let miCommand = "var-update ";
		if (threadId != 0) {
			miCommand += `--thread ${threadId} --frame ${frameLevel}`;
		}
		return this.sendCommand(`${miCommand} --all-values ${this.quote(name)}`);
	}

	async varAssign(name: string, rawValue: string): Promise<MINode> {
		if (trace)
			this.log("stderr", "varAssign");
		return this.sendCommand(`var-assign ${this.quote(name)} ${rawValue}`);
	}

	logNoNewLine(type: string, msg: string) {
		this.emit("msg", type, msg);
	}

	log(type: string, msg: string) {
		this.emit("msg", type, msg[msg.length - 1] == '\n' ? msg : (msg + "\n"));
	}

	sendUserInput(command: string, threadId: number = 0, frameLevel: number = 0): Thenable<MINode> {
		if (command.startsWith("-")) {
			return this.sendCommand(command.substring(1));
		} else {
			return this.sendCliCommand(command, threadId, frameLevel);
		}
	}

	sendRaw(raw: string) {
		if (this.printCalls)
			this.log("log", raw);
		if (this.isSSH)
			this.stream.write(raw + "\n");
		else
			this.process.stdin.write(raw + "\n");
	}

	sendCliCommand(command: string, threadId: number = 0, frameLevel: number = 0): Thenable<MINode> {
		let miCommand = "interpreter-exec ";
		if (threadId != 0) {
			miCommand += `--thread ${threadId} --frame ${frameLevel} `;
		}
		miCommand += `console "${command.replace(/[\\"']/g, '\\$&')}"`;
		return this.sendCommand(miCommand);
	}

	sendCommand(command: string, suppressFailure: boolean = true): Thenable<MINode> {
		const sel = this.currentToken++;
		return new Promise((resolve, reject) => {
			this.handlers[sel] = (node: MINode) => {
				if (node && node.resultRecords && node.resultRecords.resultClass === "error") {
					if (suppressFailure) {
						this.log("stderr", `WARNING: Error executing command '${command}'`);
						resolve(node);
					} else
						reject(new MIError(node.result("msg") || "Internal error", command));
				} else
					resolve(node);
			};
			this.sendRaw(sel + "-" + command);
		});
	}

	isReady(): boolean {
		return this.isSSH ? this.sshReady : !!this.process;
	}

	protected quote(text: string): string {
		// only escape if text contains non-word or non-path characters such as whitespace or quotes
		return /^-|[^\w\d\/_\-\.]/g.test(text) ? ('"' + escape(text) + '"') : text;
	}

	prettyPrint: boolean = true;
	frameFilters: boolean = true;
	printCalls: boolean;
	debugOutput: boolean;
	features: string[];
	public procEnv: any;
	protected isSSH: boolean;
	protected sshReady: boolean;
	protected currentToken: number = 1;
	protected handlers: { [index: number]: (info: MINode) => any } = {};
	// path+line ->single breakpoints
	public breakpoints: Map<string, Map<string, SingleBreakpoint>> = new Map();
	protected buffer: string;
	protected errbuf: string;
	protected process: ChildProcess.ChildProcess;
	protected stream;
	protected sshConn;
	protected vscodeFrameToDDBFrame: ThreadFrameMapper;

}
type ThreadFrameKey = {
	thread: string;
	frame: string;
};

type SessionThreadFrame = {
	session: string;
	thread: string;
	frame: string;
};

class ThreadFrameMapper {
	private map: Map<string, SessionThreadFrame>;

	constructor() {
		this.map = new Map<string, SessionThreadFrame>();
	}

	private getKey(key: ThreadFrameKey): string {
		return `${key.thread}|${key.frame}`;
	}

	set(from: ThreadFrameKey, to: SessionThreadFrame): void {
		const key = this.getKey(from);
		this.map.set(key, to);
	}

	get(key: ThreadFrameKey): SessionThreadFrame | undefined {
		const mapKey = this.getKey(key);
		return this.map.get(mapKey);
	}

	has(key: ThreadFrameKey): boolean {
		const mapKey = this.getKey(key);
		return this.map.has(mapKey);
	}

	delete(key: ThreadFrameKey): boolean {
		const mapKey = this.getKey(key);
		return this.map.delete(mapKey);
	}

	getAllMappings(): Array<{ from: ThreadFrameKey; to: SessionThreadFrame }> {
		return Array.from(this.map.entries()).map(([key, value]) => {
			const [thread, frame] = key.split('|');
			return {
				from: { thread, frame },
				to: value
			};
		});
	}
}