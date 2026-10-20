package main

func routes() {
	http.HandleFunc("/tasks", getTask)
}
