using System;
using Acme.Core;

namespace Acme.App
{
    public static class Program
    {
        public static void Main(string[] args)
        {
            var repo = new Repo();
            repo.PersistEntity();
            Helper();
            Console.WriteLine("up");
        }

        static void Helper() { }
    }
}
