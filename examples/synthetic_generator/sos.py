import asyncio
import httpx

TIMEOUT = 300

class SoSAPIError(Exception):
    def __init__(self, status_code: int, message: str, endpoint: str):
        super().__init__(f"HTTP {status_code} when calling {endpoint}: {message}")
        self.status_code = status_code
        self.message = message
        self.endpoint = endpoint

class SoSBadRequestError(SoSAPIError):
    pass
class SoSNotFoundError(SoSAPIError):
    pass
class SoSTimeoutError(SoSAPIError):
    pass
class SoSInternalServerError(SoSAPIError):
    pass


class SoSClient:
  def __init__(self, server_url="http://localhost:3000"):
    self.server_url = server_url

  def _raise_api_error(self, response: httpx.Response, path: str, context: str | None = None):
      status = response.status_code
      message = response.text.strip()
      if context:
          message = f"{context}: {message}"
      if status == 400:
          raise SoSBadRequestError(status, message, path)
      if status == 404:
          raise SoSNotFoundError(status, message, path)
      if status == 504:
          raise SoSTimeoutError(status, message, path)
      if status >= 500:
          raise SoSInternalServerError(status, message, path)
      raise SoSAPIError(status, message, path)

  async def _request(self, method: str, path: str, json: dict | None = None) -> httpx.Response:
      url = f"{self.server_url}{path}"
      async with httpx.AsyncClient(timeout=TIMEOUT) as client:
          response = await client.request(method, url, json=json)
      if response.status_code >= 400:
          self._raise_api_error(response, path)
      return response

  async def create_sandbox(self, image="ubuntu:latest", setup_commands=None):
      """
      Creates a new sandbox.

      Args:
          image (str): The container image to use.
          setup_commands (list): A list of commands to run after the container starts.
      Returns:
          str: The ID of the created sandbox.
      """
      if setup_commands is None:
          setup_commands = []
      payload = {"image": image, "setup_commands": setup_commands}
      response = await self._request("POST", "/sandboxes", json=payload)
      parsed = response.json()
      if "id" not in parsed:
        raise SoSAPIError(response.status_code, f"Missing id in response: {parsed}", "/sandboxes")
      return parsed["id"]

  async def list_sandboxes(self):
      """
      Lists all available sandboxes.

      Returns:
          list: A list of dictionaries, each containing information about a sandbox.
      """
      response = await self._request("GET", "/sandboxes")
      return response.json()

  async def start_sandbox(self, sandbox_id):
      """ Starts a specific sandbox.  """
      await self._request("POST", f"/sandboxes/{sandbox_id}/start")

  async def exec_command(self, sandbox_id, command, standalone=False):
      """
      Executes a command in a specific sandbox.

      Args:
          sandbox_id (str): The ID of the sandbox to execute the command in.
          command (str): The command to execute.
          standalone (bool): Whether to execute the command in standalone mode.

      Returns:
          tuple[str, int]: The output and exit code from the sandbox.
      """
      payload = {"command": command, "standalone": bool(standalone)}
      path = f"/sandboxes/{sandbox_id}/exec"
      url = f"{self.server_url}{path}"
      async with httpx.AsyncClient(timeout=TIMEOUT) as client:
          response = await client.post(url, json=payload)
      if response.status_code >= 400:
          self._raise_api_error(response, path, context=f"command={command}")
      parsed = response.json()
      if "output" not in parsed or "exit_code" not in parsed or "exited" not in parsed:
          raise SoSAPIError(response.status_code, f"Missing fields in response: {parsed}", path)
      return parsed["output"], parsed["exit_code"], parsed["exited"]

  async def stop_sandbox(self, sandbox_id, remove=True):
      """
      Stops and removes a specific sandbox.

      Args:
          sandbox_id (str): The ID of the sandbox to stop.
          server_url (str): The base URL of the sandbox server.
      """
      await self._request("POST", f"/sandboxes/{sandbox_id}/stop", json={'remove': remove})
    
  async def get_sandbox_trajectory(self, sandbox_id, formatted=False):
      """
      Gets the trajectory of a specific sandbox.

      Args:
          sandbox_id (str): The ID of the sandbox to get the trajectory of.
          formatted (bool): Whether to format the trajectory as a list of commands.
      """
      response = await self._request("GET", f"/sandboxes/{sandbox_id}/trajectory{'/formatted' if formatted else ''}")
      trajectory = response.text if formatted else response.json() 
      return trajectory

if __name__ == "__main__":
    sos = SoSClient()
    async def main():
        sandbox_id = await sos.create_sandbox(image="shellm-sandbox:latest", setup_commands=["echo 'Hello, world!'"])
        try:
            await sos.start_sandbox(sandbox_id)
            _, code = await sos.exec_command(sandbox_id, "# Let me list the files here")
            assert code == 0, "Failed to execute command"
            _, code = await sos.exec_command(sandbox_id, "ls -la")
            assert code == 0, "Failed to execute command"
            trajectory = await sos.get_sandbox_trajectory(sandbox_id, formatted=True)
            print(trajectory)
        except Exception as e:
            print(e)
            raise e
        finally:
            await sos.stop_sandbox(sandbox_id, remove=False)
    asyncio.run(main())
