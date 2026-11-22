namespace App;

public class Program
{
    private Square square = new Square();

    public void Run()
    {
        this.square.Describe();
    }
}

public class Failure : System.Exception
{
    public void Show()
    {
        Describe();
    }
}
