namespace Acme.Core
{
    public class Repo
    {
        public void PersistEntity() { Validate(); }

        private void Validate() { }

        public static string Describe(int id) { return id.ToString(); }

        public override string ToString() { return "repo"; }
    }
}
