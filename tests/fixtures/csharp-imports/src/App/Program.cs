using System;
using System.Collections.Generic;
using Microsoft.Extensions.Logging;
using Newtonsoft.Json.Linq;
using Serilog;
using Acme.Core;
using Acme.Core.Models;
using static Acme.Core.Util.MathHelpers;
using static Acme.Core.Models.Outer.Nested;
using Json = Newtonsoft.Json.JsonConvert;
using Repo = Acme.Data.Repository<int>;
using CoreNs = Acme.Core.Util;
using Acme.Missing;
using Acme;
using Xunit;

namespace Acme.App
{
    using Services;

    public static class Program
    {
        public static void Main() { using (var x = new System.IO.MemoryStream()) { } }
    }
}
