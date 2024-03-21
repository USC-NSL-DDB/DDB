package main

import (
    "fmt"
    "sync"
)

const NUM_GOROUTINE = 10

func printHello(wg *sync.WaitGroup, idx int) {
  defer wg.Done() // Notify the WaitGroup that this goroutine is done
  fmt.Printf("Hello, World! Index: %d\n", idx)
}

func main() {
  var wg sync.WaitGroup

  for i := range(NUM_GOROUTINE) {
    wg.Add(1) // Add one goroutine to the WaitGroup
    go printHello(&wg, i) // Spawn the goroutine, passing the WaitGroup by reference
  }
  wg.Wait()
  fmt.Println("All goroutines finished.")
}

