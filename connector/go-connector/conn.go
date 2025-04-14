package main

import (
	"crypto/sha256"
	"fmt"
	"io"
	"log"
	"net"
	"os"
	"os/signal"
	"runtime"
	"strconv"
	"strings"
	"syscall"
	"time"

	mqtt "github.com/eclipse/paho.mqtt.golang"
)

// --- Configuration (Defaults & Environment Variable Names) ---
const (
	envIP              = "DDB_IPV4"
	envTag             = "DDB_TAG"
	envAlias           = "DDB_ALIAS"
	envMQTTBroker      = "DDB_MQTT_BROKER" // e.g., "tcp://localhost:1883"
	envMQTTTopic       = "DDB_MQTT_TOPIC"  // e.g., "ddb/discovery"
	envMQTTUser        = "DDB_MQTT_USER"
	envMQTTPass        = "DDB_MQTT_PASS"
	envAutoDiscovery   = "DDB_AUTO_DISCOVERY"
	envWaitForAttach   = "DDB_WAIT_FOR_ATTACH"
	envSignalToWaitFor = "DDB_SIGNAL" // Signal name (e.g., "SIGUSR1") or number (e.g., "40")

	defaultTag           = "proc"
	defaultAlias         = "bin"
	defaultAutoDiscovery = true
	defaultWaitForAttach = true
	defaultSignalStr     = "SIGUSR1" // Default signal

	mqttClientIDPrefix = "ddb_client_"
	mqttConnectTimeout = 5 * time.Second
	mqttPublishTimeout = 5 * time.Second
	mqttDisconnectQ    = 250 // milliseconds to wait for disconnect
)

// Global MQTT client instance
var mqttClient mqtt.Client

// Signal to wait for debugger attachment
var signalToWaitFor syscall.Signal = syscall.SIGUSR1 // Default, updated in init

// --- init() function: Executed automatically on package import ---
func init() {
	log.Println("Initializing DDB connector...")

	// --- Parse Configuration ---
	config := loadConfig()
	log.Printf("DDB Config: %+v\n", config)

	// Determine the signal to wait for
	signalToWaitFor = parseSignal(config.SignalStr)
	log.Printf("DDB: Configured to wait for signal %v (%d)\n", signalToWaitFor, signalToWaitFor)

	// --- Get Process Info ---
	pid := os.Getpid()
	ip := config.IP // Use configured IP first
	if ip == "" {
		var err error
		ip, err = getNonLoopbackIP()
		if err != nil {
			log.Printf("DDB WARN: Could not automatically determine non-loopback IP: %v. Discovery might fail.", err)
			// Proceeding without IP might be okay if only signal handling is needed
		}
	}
	log.Printf("DDB Info: PID=%d, IP=%s\n", pid, ip)

	// --- Auto Discovery & MQTT Reporting ---
	if config.AutoDiscovery && ip != "" && config.MQTTBroker != "" && config.MQTTTopic != "" {
		execHash, err := getExecutableHash()
		if err != nil {
			log.Printf("DDB WARN: Failed to get executable hash: %v", err)
			execHash = "unknown" // Proceed without hash if failed
		}

		// Connect to MQTT
		opts := mqtt.NewClientOptions().
			AddBroker(config.MQTTBroker).
			SetClientID(fmt.Sprintf("%s%d", mqttClientIDPrefix, pid)).
			SetConnectTimeout(mqttConnectTimeout).
			SetAutoReconnect(false) // Disable auto-reconnect for simplicity in init

		if config.MQTTUser != "" {
			opts.SetUsername(config.MQTTUser)
			opts.SetPassword(config.MQTTPass)
		}

		mqttClient = mqtt.NewClient(opts)
		if token := mqttClient.Connect(); token.Wait() && token.Error() != nil {
			log.Printf("DDB ERROR: Failed to connect to MQTT broker %s: %v", config.MQTTBroker, token.Error())
			// Don't proceed with publishing if connection failed
		} else {
			log.Printf("DDB: Connected to MQTT broker %s", config.MQTTBroker)
			// Successfully connected, schedule disconnect on exit
			setupShutdownHandler() // Sets up SIGINT/SIGTERM listener

			// Publish discovery message
			payload := fmt.Sprintf("%s:%s:%d:%s=%s", ip, config.Tag, pid, execHash, config.Alias)
			token := mqttClient.Publish(config.MQTTTopic, 1, false, payload) // QoS 1

			// Wait briefly for publish, but don't block init forever
			if token.WaitTimeout(mqttPublishTimeout) && token.Error() != nil {
				log.Printf("DDB ERROR: Failed to publish discovery message: %v", token.Error())
			} else if token.Error() == nil {
				log.Printf("DDB: Service discovery message published to %s: %s", config.MQTTTopic, payload)
			} else {
				// Timeout case
				log.Printf("DDB WARN: MQTT publish timed out after %v", mqttPublishTimeout)
			}

			// --- Wait for Attach (if configured) ---
			// IMPORTANT: This must run AFTER successful connection/publish
			// and must be in a goroutine to avoid blocking application startup
			if config.WaitForAttach {
				waitForSignalAndTrap(pid) // Launch background waiter
			}
		}
	} else if config.WaitForAttach {
		// If discovery is off/failed but waiting is still requested
		log.Println("DDB: Auto-discovery disabled or failed, but wait_for_attach is true. Setting up signal waiter.")
		waitForSignalAndTrap(pid) // Launch background waiter
	} else {
		log.Println("DDB: Auto-discovery and wait_for_attach are disabled.")
	}

	log.Println("DDB connector initialization sequence finished.")
	// init() returns here, application continues. Goroutines run in background.
}

// --- Helper Functions ---

type ddbConfig struct {
	IP            string
	Tag           string
	Alias         string
	MQTTBroker    string
	MQTTTopic     string
	MQTTUser      string
	MQTTPass      string
	AutoDiscovery bool
	WaitForAttach bool
	SignalStr     string
}

func loadConfig() ddbConfig {
	return ddbConfig{
		IP:            os.Getenv(envIP), // Empty string means auto-detect later
		Tag:           getEnvOrDefault(envTag, defaultTag),
		Alias:         getEnvOrDefault(envAlias, defaultAlias),
		MQTTBroker:    os.Getenv(envMQTTBroker),
		MQTTTopic:     os.Getenv(envMQTTTopic),
		MQTTUser:      os.Getenv(envMQTTUser),
		MQTTPass:      os.Getenv(envMQTTPass),
		AutoDiscovery: getEnvBoolOrDefault(envAutoDiscovery, defaultAutoDiscovery),
		WaitForAttach: getEnvBoolOrDefault(envWaitForAttach, defaultWaitForAttach),
		SignalStr:     getEnvOrDefault(envSignalToWaitFor, defaultSignalStr),
	}
}

func getEnvOrDefault(key, defaultValue string) string {
	if value, exists := os.LookupEnv(key); exists {
		return value
	}
	return defaultValue
}

func getEnvBoolOrDefault(key string, defaultValue bool) bool {
	if value, exists := os.LookupEnv(key); exists {
		// Treat "false", "0", "no" as false, everything else as true
		lowerVal := strings.ToLower(value)
		return lowerVal != "false" && lowerVal != "0" && lowerVal != "no"
	}
	return defaultValue
}

// parseSignal converts a string (name or number) to a syscall.Signal
func parseSignal(sigStr string) syscall.Signal {
	// Try parsing as a number first
	if sigNum, err := strconv.Atoi(sigStr); err == nil {
		// Check if it's a valid signal number (platform-dependent check might be needed)
		// For simplicity, assume positive numbers are potentially valid
		if sigNum > 0 {
			log.Printf("DDB: Interpreting signal '%s' as number %d\n", sigStr, sigNum)
			return syscall.Signal(sigNum)
		}
	}

	// Try mapping common signal names (add more as needed)
	// Ensure consistent naming (e.g., SIGUSR1 vs USR1)
	upperSigStr := strings.ToUpper(sigStr)
	if !strings.HasPrefix(upperSigStr, "SIG") {
		upperSigStr = "SIG" + upperSigStr
	}

	switch upperSigStr {
	case "SIGHUP":
		return syscall.SIGHUP
	case "SIGINT":
		return syscall.SIGINT
	case "SIGQUIT":
		return syscall.SIGQUIT
	case "SIGILL":
		return syscall.SIGILL
	case "SIGTRAP":
		return syscall.SIGTRAP
	case "SIGABRT", "SIGIOT":
		return syscall.SIGABRT
	case "SIGBUS":
		return syscall.SIGBUS
	case "SIGFPE":
		return syscall.SIGFPE
	case "SIGKILL":
		return syscall.SIGKILL
	case "SIGUSR1":
		return syscall.SIGUSR1
	case "SIGSEGV":
		return syscall.SIGSEGV
	case "SIGUSR2":
		return syscall.SIGUSR2
	case "SIGPIPE":
		return syscall.SIGPIPE
	case "SIGALRM":
		return syscall.SIGALRM
	case "SIGTERM":
		return syscall.SIGTERM
	// Add other signals as needed
	default:
		log.Printf("DDB WARN: Unknown signal string '%s'. Defaulting to SIGUSR1.", sigStr)
		return syscall.SIGUSR1
	}
}

func getNonLoopbackIP() (string, error) {
	addrs, err := net.InterfaceAddrs()
	if err != nil {
		return "", fmt.Errorf("failed to get interface addresses: %w", err)
	}

	for _, address := range addrs {
		// Check the address type and if it is not a loopback
		if ipnet, ok := address.(*net.IPNet); ok && !ipnet.IP.IsLoopback() {
			if ipnet.IP.To4() != nil {
				return ipnet.IP.String(), nil // Found a non-loopback IPv4
			}
		}
	}
	return "", fmt.Errorf("no non-loopback IPv4 address found")
}

func getExecutableHash() (string, error) {
	exePath, err := os.Executable()
	if err != nil {
		return "", fmt.Errorf("failed to get executable path: %w", err)
	}

	file, err := os.Open(exePath)
	if err != nil {
		return "", fmt.Errorf("failed to open executable '%s': %w", exePath, err)
	}
	defer file.Close()

	hasher := sha256.New()
	if _, err := io.Copy(hasher, file); err != nil {
		return "", fmt.Errorf("failed to read executable '%s' for hashing: %w", exePath, err)
	}

	return fmt.Sprintf("%x", hasher.Sum(nil)), nil
}

// waitForSignalAndTrap runs in a goroutine, waits for the configured signal, then breaks.
func waitForSignalAndTrap(pid int) {
	sigChan := make(chan os.Signal, 1)
	// Notify for the specific signal determined during init
	signal.Notify(sigChan, signalToWaitFor)

	log.Printf("DDB: Process PID %d waiting for signal %v (%d) to continue/break...", pid, signalToWaitFor, signalToWaitFor)

	// Block until the signal is received
	receivedSig := <-sigChan
	log.Printf("DDB: Received signal %v. Triggering breakpoint.", receivedSig)

	// Stop execution for the debugger
	runtime.Breakpoint()

	// Code here will execute only if the debugger resumes execution
	log.Printf("DDB: Resumed execution after breakpoint.")
	// Optionally, stop listening? Or allow multiple breaks?
	// signal.Stop(sigChan)
	// close(sigChan)
}

// setupShutdownHandler listens for SIGINT/SIGTERM for graceful MQTT disconnect.
func setupShutdownHandler() {
	sigChan := make(chan os.Signal, 1)
	signal.Notify(sigChan, syscall.SIGINT, syscall.SIGTERM)

	// This goroutine waits for a shutdown signal
	go func() {
		sig := <-sigChan
		log.Printf("DDB: Received shutdown signal %v. Disconnecting MQTT.", sig)
		if mqttClient != nil && mqttClient.IsConnected() {
			mqttClient.Disconnect(mqttDisconnectQ)
			log.Println("DDB: MQTT client disconnected.")
		}
		// Note: Depending on application structure, may need os.Exit here
		// or allow main to exit naturally.
	}()
}
