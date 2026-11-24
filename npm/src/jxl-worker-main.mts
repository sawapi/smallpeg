// The script createWorker() of smallpeg/jxl/worker starts: the browser
// build of smallpeg/jxl, served over the protocol in protocol.mts. Not an
// entry point of its own.

import * as web from "./jxl-web.mjs";
import { jxlNames, type Port, serveFns } from "./protocol.mjs";

serveFns(self as unknown as Port, web, (wasm) => web.init(wasm as never), jxlNames);
