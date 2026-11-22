using Acme.Core;

namespace Acme.App
{
    [Route("api/[controller]")]
    public class UsersController : ControllerBase
    {
        [HttpGet("{id}")]
        public string Get(int id)
        {
            return Repo.Describe(id);
        }
    }
}
