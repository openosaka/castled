# Castle

Castle is a simple tunnel based on GRPC that allows you to expose your local services to the internet,
but it's **mainly designed for 🌟testing and ✨development purposes**.

It resolves the problem of the traffic inside k8s reach your local services, the great
advantage of this idea is that you can mocking any external service in your `_test` file, no matter
which language you are using.

You can use this tool when you are considering:

- I want to expose my local service to the kubernetes cluster.
	- I want to mock a external service(e.g. Google, Slack, etc) when I'm doing integration tests.

Basically, this tunnel is primarily for this purpose. If you want to expose your local service to the internet,
[ngrok][ngrok], [frp][frp] or other tools are more suitable for you.


[ngrok]: https://ngrok.com/
[frp]: https://github.com/fatedier/frp


## Features

- Tcp tunnel
	- specify the remote port
	- random remote port if not specified
- Udp tunnel
	- specify the remote port
	- random remote port if not specified
- Http tunnel
	- specify the domain
	- specify the subdomain
	- specify the remote port
	- random subdomain if `--random-subdomain` is specified
	- random remote port if not specified
	- support http/1.1
	- [ ] support http/2
