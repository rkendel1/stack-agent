package main

func routes(a *echo.Group) {
	a.GET("/tasks/:id", getTask)
}
