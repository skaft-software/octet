from octet_extension import Extension

ext = Extension(api_version="0.4", max_concurrent_requests=1)

@ext.tool(name="wait", description="Wait in short, cancellable intervals")
def wait(args):
    for _ in range(args.get("steps", 20)):
        ext.cancellation.raise_if_cancelled()
        ext.cancellation.wait(0.1)
    return "Finished."

ext.run()
