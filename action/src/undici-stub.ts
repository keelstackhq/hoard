// The build aliases `undici` to this file. @actions/http-client imports undici's ProxyAgent only
// for HttpClient.getAgentDispatcher(), which neither @actions/core nor @actions/tool-cache call;
// bundling the real undici (~400 KB) would break the 200 KB action/dist budget.
export class ProxyAgent {
  constructor() {
    throw new Error("undici is not bundled in the hoard action");
  }
}
