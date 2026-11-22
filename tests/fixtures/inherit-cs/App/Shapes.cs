namespace App;

public interface IShape
{
    double Area();
}

public abstract class ShapeBase : IShape
{
    public abstract double Area();

    public string Describe()
    {
        return Area().ToString();
    }

    protected virtual void Log() { }
}

public class Square : ShapeBase, IShape
{
    public override double Area()
    {
        Log();
        this.Describe();
        base.Log();
        return 1;
    }
}
